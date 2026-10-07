//! Clipboard platform adapter primitives.

use arboard::Clipboard;

pub(crate) fn copy_text(text: String) -> Result<(), String> {
    Clipboard::new()
        .map_err(|_| "clipboard_unavailable".to_string())?
        .set_text(text)
        .map_err(|_| "clipboard_write_failed".to_string())
}

/// Reads the system clipboard's monotonic change counter without exposing
/// pasteboard SDK types to the recorder/collector business logic.
#[cfg(target_os = "macos")]
pub(crate) fn pasteboard_change_count() -> i64 {
    use objc2_app_kit::NSPasteboard;

    NSPasteboard::generalPasteboard().changeCount() as i64
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn pasteboard_change_count() -> i64 {
    0
}
