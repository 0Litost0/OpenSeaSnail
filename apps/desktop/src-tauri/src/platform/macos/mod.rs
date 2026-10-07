//! macOS-only adapter namespace.
//!
//! Capabilities that require AppKit, AVFoundation, CoreGraphics or TCC remain
//! behind this target boundary; portable adapter code imports only semantic
//! functions from the parent `platform` modules.

use objc2_app_kit::{
    NSScreenSaverWindowLevel, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSString, NSURL};
use tauri::WebviewWindow;
use tauri_nspanel::{Panel, WebviewWindowExt};

mod recording_capsule_panel {
    use super::*;
    use tauri::Manager;

    tauri_nspanel::tauri_panel! {
        RecordingCapsulePanel {
            config: {
                can_become_key_window: false,
                can_become_main_window: false,
                is_floating_panel: true
            }
        }
    }

    pub(super) fn configure_window(window: &WebviewWindow) -> Result<(), String> {
        let panel = window
            .to_panel::<RecordingCapsulePanel>()
            .map_err(|err| format!("无法将录制胶囊转换为 NSPanel: {err}"))?;
        super::configure_recording_capsule_panel(panel.as_ref());
        Ok(())
    }
}

pub(crate) fn configure_recording_capsule_window(window: &WebviewWindow) -> Result<(), String> {
    recording_capsule_panel::configure_window(window)
}

pub(crate) fn configure_recording_capsule_panel(panel: &dyn Panel) {
    panel.set_style_mask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
    panel.set_floating_panel(true);
    panel.set_hides_on_deactivate(false);
    panel.set_level(NSScreenSaverWindowLevel as i64);
}

pub(crate) fn configure_recording_capsule_fullscreen_behavior(
    window: &WebviewWindow,
) -> Result<(), String> {
    let native_window = window
        .ns_window()
        .map_err(|err| format!("无法获取录制胶囊原生窗口: {err}"))?;
    // setup 在 macOS 主线程中执行；NSWindow 仅在该线程访问。
    let native_window = unsafe { &*native_window.cast::<NSWindow>() };
    let behavior = (native_window.collectionBehavior()
        & !(NSWindowCollectionBehavior::FullScreenPrimary
            | NSWindowCollectionBehavior::FullScreenNone
            | NSWindowCollectionBehavior::Primary
            | NSWindowCollectionBehavior::Auxiliary
            | NSWindowCollectionBehavior::Managed
            | NSWindowCollectionBehavior::Transient))
        | NSWindowCollectionBehavior::CanJoinAllSpaces
        | NSWindowCollectionBehavior::CanJoinAllApplications
        | NSWindowCollectionBehavior::FullScreenAuxiliary
        | NSWindowCollectionBehavior::Stationary
        | NSWindowCollectionBehavior::IgnoresCycle;
    native_window.setCollectionBehavior(behavior);
    native_window.setLevel(NSScreenSaverWindowLevel);
    Ok(())
}

pub(crate) fn bring_recording_capsule_to_front(window: &WebviewWindow) -> Result<(), String> {
    let window = window.clone();
    let window_for_main_thread = window.clone();
    window_for_main_thread
        .run_on_main_thread(move || {
            if let Err(error) = configure_recording_capsule_fullscreen_behavior(&window) {
                eprintln!("无法配置录制胶囊全屏行为: {error}");
                return;
            }
            let Ok(native_window) = window.ns_window() else {
                eprintln!("无法获取录制胶囊原生窗口");
                return;
            };
            let native_window = unsafe { &*native_window.cast::<NSWindow>() };
            native_window.orderFrontRegardless();
        })
        .map_err(|err| format!("无法将录制胶囊置于全屏应用前方: {err}"))
}

pub(crate) fn post_command_v() -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| "无法创建 CGEvent source".to_string())?;
    let down = CGEvent::new_keyboard_event(source.clone(), 9, true)
        .map_err(|_| "无法创建 Cmd+V keyDown".to_string())?;
    let up = CGEvent::new_keyboard_event(source, 9, false)
        .map_err(|_| "无法创建 Cmd+V keyUp".to_string())?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);
    up.post(CGEventTapLocation::HID);
    Ok(())
}

pub(crate) fn frontmost_process_id() -> Option<i32> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|application| application.processIdentifier())
        .filter(|pid| *pid > 0)
}

pub(crate) fn open_with_default_application(target: &str, is_file: bool) -> Result<bool, String> {
    let value = NSString::from_str(target);
    let url = if is_file {
        NSURL::fileURLWithPath(&value)
    } else {
        NSURL::URLWithString(&value).ok_or_else(|| "invalid_url".to_string())?
    };
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(true)
    } else {
        Err("open_failed".into())
    }
}
