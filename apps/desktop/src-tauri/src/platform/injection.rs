//! Daemon 注入计划和 macOS 文本副作用 adapter。

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InjectionOutcome {
    Pasted {
        method: String,
        clipboard_restored: bool,
    },
    ClipboardOnly {
        reason: String,
    },
    Failed {
        code: String,
        paste_dispatched: bool,
        clipboard_written: bool,
        clipboard_restored: bool,
    },
}

use crate::clipboard_collector;
use crate::platform::permission::accessibility_permission_granted;
use crate::InjectionPlan;
use arboard::{Clipboard, ImageData};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
const PASTE_DELAY: Duration = Duration::from_millis(120);
const CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(450);
/// 剪贴板操作必须串行：一次注入的延迟恢复不能覆盖下一次注入或用户的新复制。
pub(crate) struct TextInjector {
    operation: Mutex<()>,
}

/// MVP 覆盖系统常见的富文本、图片和纯文本；未知格式宁可不恢复，也不伪造内容。
enum ClipboardSnapshot {
    Html {
        html: String,
        alt_text: Option<String>,
    },
    Image(ImageData<'static>),
    Text(String),
}

fn snapshot_clipboard(clipboard: &mut Clipboard) -> Option<ClipboardSnapshot> {
    // ST-M4.5：文件复制（file URL flavor）无法快照与恢复 → 不恢复，避免恢复成纯文本
    // 路径而丢失 file URL、损坏用户原始文件复制（files 与其他 flavor 共存亦同）。
    let has_files = clipboard
        .get()
        .file_list()
        .map(|paths| !paths.is_empty())
        .unwrap_or(false);
    if has_files {
        return None;
    }
    let html = clipboard.get().html().ok();
    let image = clipboard.get_image().ok();
    // 多类型（html+image 共存）无法完整快照 → 不恢复，避免部分恢复损坏原剪贴板
    //（宁可保留注入内容，也不伪造或破坏用户原始多类型剪贴板）。
    if html.is_some() && image.is_some() {
        return None;
    }
    if let Some(html) = html {
        return Some(ClipboardSnapshot::Html {
            html,
            alt_text: clipboard.get_text().ok(),
        });
    }
    if let Some(image) = image {
        return Some(ClipboardSnapshot::Image(image));
    }
    clipboard.get_text().ok().map(ClipboardSnapshot::Text)
}

fn restore_clipboard(clipboard: &mut Clipboard, snapshot: ClipboardSnapshot) -> Result<(), String> {
    match snapshot {
        ClipboardSnapshot::Html { html, alt_text } => clipboard
            .set_html(html, alt_text)
            .map_err(|err| format!("粘贴完成但无法恢复富文本剪贴板: {err}")),
        ClipboardSnapshot::Image(image) => clipboard
            .set_image(image)
            .map_err(|err| format!("粘贴完成但无法恢复图片剪贴板: {err}")),
        ClipboardSnapshot::Text(text) => clipboard
            .set_text(text)
            .map_err(|err| format!("粘贴完成但无法恢复原剪贴板: {err}")),
    }
}

/// 只在注入文本仍是当前剪贴板内容时恢复，绝不覆盖用户在等待期间的新复制。
fn should_restore_clipboard(
    has_snapshot: bool,
    current_text: Option<&str>,
    injected_text: &str,
) -> bool {
    has_snapshot && current_text == Some(injected_text)
}

/// ST-M3.4：注入计划归一摘要（与 collector 同款 `content_digest`）：html 非空→RichText，
/// 否则 PlainText。注入器据此注册写 #1（set_html/set_text）的自写摘要。
fn plan_digest(plan: &InjectionPlan) -> u64 {
    if plan.html.is_empty() {
        clipboard_collector::content_digest(&clipboard_collector::NormalizedContent::PlainText(
            plan.plain.clone(),
        ))
    } else {
        clipboard_collector::content_digest(&clipboard_collector::NormalizedContent::RichText {
            plain: plan.plain.clone(),
            html: plan.html.clone(),
        })
    }
}

/// ST-M3.4：注入前快照的归一摘要——恢复写（写 #2）把该内容写回剪贴板，故须同款摘要
/// 注册以抑制。Html→RichText（plain=alt_text）、Text→PlainText、Image→Image（像素同款）。
fn snapshot_digest(snapshot: &ClipboardSnapshot) -> Option<u64> {
    match snapshot {
        ClipboardSnapshot::Html { html, alt_text } => Some(clipboard_collector::content_digest(
            &clipboard_collector::NormalizedContent::RichText {
                plain: alt_text.clone().unwrap_or_default(),
                html: html.clone(),
            },
        )),
        ClipboardSnapshot::Text(text) => Some(clipboard_collector::content_digest(
            &clipboard_collector::NormalizedContent::PlainText(text.clone()),
        )),
        ClipboardSnapshot::Image(image) => Some(clipboard_collector::content_digest(
            &clipboard_collector::NormalizedContent::Image {
                width: image.width,
                height: image.height,
                rgba: image.bytes.as_ref().to_vec(),
            },
        )),
    }
}

impl TextInjector {
    pub(crate) fn new() -> Self {
        Self {
            operation: Mutex::new(()),
        }
    }

    /// ST-M4.5：富文本注入计划。`html` 非空时同时写 `text/html`+`text/plain`（arboard
    /// `set_html` 一次 NSPasteboard changeCount 递增；目标不支持 HTML 时消费 plain），
    /// 否则退回 `set_text`。复用既有 snapshot/CGEvent/恢复语义；恢复判定按
    /// `current_text == plain`（`set_html` 后 `get_text` 返回 alt_text=plain，故成立）。
    ///
    /// ST-M3.4：`register` 在写剪贴板前为每次自写注册摘要——注入含两次连续自写（set_html
    /// 写入 + 恢复原剪贴板），故注册两个摘要（plan + previous），poll_loop 各匹配移除，
    /// 既不漏抑制恢复写、也不忽略随后用户复制。
    pub(crate) fn inject_plan(
        &self,
        plan: &InjectionPlan,
        keep_transcription_in_clipboard: bool,
        mut register: impl FnMut(u64),
    ) -> InjectionOutcome {
        // Retain the ticket with the plan for the post-paste observer. Learning
        // failures must never block the paste path itself.
        let _learning_ticket = plan.learning_ticket.as_str();
        if plan.plain.trim().is_empty() {
            return InjectionOutcome::Failed {
                code: "recording_auto_paste_failed".into(),
                paste_dispatched: false,
                clipboard_written: false,
                clipboard_restored: false,
            };
        }
        let _operation = self.operation.lock().expect("clipboard operation mutex");
        let mut clipboard = match Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(_) => {
                return InjectionOutcome::Failed {
                    code: "recording_auto_paste_failed".into(),
                    paste_dispatched: false,
                    clipboard_written: false,
                    clipboard_restored: false,
                };
            }
        };
        if !accessibility_permission_granted() {
            return match clipboard.set_text(&plan.plain) {
                Ok(()) => InjectionOutcome::ClipboardOnly {
                    reason: "recording_accessibility_required".into(),
                },
                Err(_) => InjectionOutcome::Failed {
                    code: "recording_auto_paste_failed".into(),
                    paste_dispatched: false,
                    clipboard_written: false,
                    clipboard_restored: false,
                },
            };
        }
        let previous = snapshot_clipboard(&mut clipboard);
        // 预注册两次自写摘要：写 #1（注入内容）+ 写 #2（恢复的原剪贴板）。
        register(plan_digest(plan));
        if !keep_transcription_in_clipboard {
            if let Some(prev) = &previous {
                if let Some(d) = snapshot_digest(prev) {
                    register(d);
                }
            }
        }
        if plan.html.is_empty() {
            if clipboard.set_text(&plan.plain).is_err() {
                return InjectionOutcome::Failed {
                    code: "recording_auto_paste_failed".into(),
                    paste_dispatched: false,
                    clipboard_written: false,
                    clipboard_restored: false,
                };
            }
        } else {
            if clipboard
                .set_html(plan.html.clone(), Some(plan.plain.clone()))
                .is_err()
            {
                return InjectionOutcome::Failed {
                    code: "recording_auto_paste_failed".into(),
                    paste_dispatched: false,
                    clipboard_written: false,
                    clipboard_restored: false,
                };
            }
        }

        thread::sleep(PASTE_DELAY);
        let method = match post_command_v() {
            Ok(()) => "cgevent",
            Err(_) => match post_command_v_applescript() {
                Ok(()) => "applescript",
                Err(_) => {
                    return InjectionOutcome::Failed {
                        code: "recording_auto_paste_failed".into(),
                        paste_dispatched: false,
                        clipboard_written: true,
                        clipboard_restored: false,
                    };
                }
            },
        };

        if keep_transcription_in_clipboard {
            return InjectionOutcome::Pasted {
                method: method.into(),
                clipboard_restored: false,
            };
        }

        thread::sleep(CLIPBOARD_RESTORE_DELAY);
        let current_text = clipboard.get_text().ok();
        let restored_clipboard =
            if should_restore_clipboard(previous.is_some(), current_text.as_deref(), &plan.plain) {
                match restore_clipboard(&mut clipboard, previous.expect("checked above")) {
                    Ok(()) => true,
                    Err(_) => {
                        return InjectionOutcome::Failed {
                            code: "clipboard_restore_failed".into(),
                            paste_dispatched: true,
                            clipboard_written: true,
                            clipboard_restored: false,
                        };
                    }
                }
            } else {
                false
            };
        InjectionOutcome::Pasted {
            method: method.into(),
            clipboard_restored: restored_clipboard,
        }
    }
}

#[cfg(target_os = "macos")]
fn post_command_v() -> Result<(), String> {
    crate::platform::macos::post_command_v()
}

#[cfg(not(target_os = "macos"))]
fn post_command_v() -> Result<(), String> {
    Err("当前平台不支持 CGEvent 注入".into())
}

fn post_command_v_applescript() -> Result<(), String> {
    let status = Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to key code 9 using command down")
        .status()
        .map_err(|err| format!("无法启动 osascript: {err}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("osascript 退出状态: {status}"))
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use crate::InjectionPlan;
    #[test]
    fn text_injector_rejects_blank_text_before_accessing_clipboard() {
        assert!(matches!(
            TextInjector::new().inject_plan(
                &InjectionPlan {
                    plain: " \n\t ".into(),
                    html: String::new(),
                    learning_ticket: "ticket".into(),
                },
                false,
                |_| {}
            ),
            InjectionOutcome::Failed {
                paste_dispatched: false,
                clipboard_written: false,
                ..
            }
        ));
    }

    #[test]
    fn clipboard_restore_preserves_user_changes() {
        assert!(should_restore_clipboard(true, Some("注入内容"), "注入内容"));
        assert!(!should_restore_clipboard(
            true,
            Some("用户新复制"),
            "注入内容"
        ));
        assert!(!should_restore_clipboard(
            false,
            Some("注入内容"),
            "注入内容"
        ));
    }
}
