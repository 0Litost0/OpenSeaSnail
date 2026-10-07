//! macOS permission adapter and stable desktop-facing permission values.

use std::process::Command;
use std::sync::mpsc;

use serde::Serialize;

#[cfg(target_os = "macos")]
use block2::RcBlock;
#[cfg(target_os = "macos")]
use objc2::runtime::Bool;
#[cfg(target_os = "macos")]
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

#[derive(Clone, Serialize)]
pub(crate) struct MicrophonePermission {
    pub(crate) granted: bool,
    pub(crate) status: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct PermissionsStatus {
    pub(crate) microphone: MicrophonePermission,
    pub(crate) accessibility_granted: bool,
}

pub(crate) fn microphone_permission() -> MicrophonePermission {
    #[cfg(target_os = "macos")]
    {
        // Apple 要求先查状态；`requestAccess` 仅应在用户实际要录制时调用。
        let media_type =
            unsafe { AVMediaTypeAudio.expect("AVFoundation 应提供 AVMediaTypeAudio") };
        let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
        microphone_permission_from_status(status)
    }
    #[cfg(not(target_os = "macos"))]
    {
        MicrophonePermission {
            granted: false,
            status: "unsupported".into(),
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn microphone_permission_from_status(
    status: AVAuthorizationStatus,
) -> MicrophonePermission {
    let (granted, status) = match status {
        AVAuthorizationStatus::Authorized => (true, "authorized"),
        AVAuthorizationStatus::NotDetermined => (false, "not_determined"),
        AVAuthorizationStatus::Denied => (false, "denied"),
        AVAuthorizationStatus::Restricted => (false, "restricted"),
        _ => (false, "unknown"),
    };
    MicrophonePermission {
        granted,
        status: status.into(),
    }
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

pub(crate) fn accessibility_permission_granted() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        // 静默检查，避免仅刷新设置页就弹出不可控的 TCC 对话框。
        AXIsProcessTrusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub(crate) fn permissions_status() -> PermissionsStatus {
    PermissionsStatus {
        microphone: microphone_permission(),
        accessibility_granted: accessibility_permission_granted(),
    }
}

/// 仅在用户主动点击设置页或首次录制引导时调用。系统对话框异步，命令等待其结果。
pub(crate) fn request_microphone_access() -> Result<MicrophonePermission, String> {
    #[cfg(target_os = "macos")]
    {
        let current = microphone_permission();
        if current.status != "not_determined" {
            return Ok(current);
        }

        let (sender, receiver) = mpsc::sync_channel(1);
        let completion: RcBlock<dyn Fn(Bool)> = RcBlock::new(move |granted: Bool| {
            let _ = sender.send(bool::from(granted));
        });
        let media_type =
            unsafe { AVMediaTypeAudio.expect("AVFoundation 应提供 AVMediaTypeAudio") };
        unsafe {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &completion);
        }
        receiver
            .recv()
            .map_err(|err| format!("等待麦克风授权结果失败: {err}"))?;
        Ok(microphone_permission())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("当前平台不支持麦克风权限申请".into())
    }
}

pub(crate) fn open_accessibility_privacy_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .status()
            .map_err(|err| format!("无法打开辅助功能设置: {err}"))?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("打开辅助功能设置失败: {status}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("当前平台不支持辅助功能设置".into())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn microphone_tcc_status_maps_to_stable_ipc_values() {
        let authorized = microphone_permission_from_status(AVAuthorizationStatus::Authorized);
        assert!(authorized.granted);
        assert_eq!(authorized.status, "authorized");
        assert_eq!(
            microphone_permission_from_status(AVAuthorizationStatus::NotDetermined).status,
            "not_determined"
        );
        assert_eq!(
            microphone_permission_from_status(AVAuthorizationStatus::Denied).status,
            "denied"
        );
    }
}
