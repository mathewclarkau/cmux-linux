//! System clipboard helpers for the TUI.
//!
//! cmux only *writes* the host clipboard via OSC 52. Reads shell out to
//! `wl-paste` (Wayland) or `xclip` (X11) so we can paste into browser panes
//! and inject clipboard images into PTY panes (issue #30 — Claude Code's
//! own clipboard read often fails inside nested terminal multiplexers).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Read text from the desktop clipboard. Returns `None` if no tool is
/// available or the clipboard is empty/unreadable.
pub fn read_text() -> Option<String> {
    let bytes = read_clipboard_bytes(&["wl-paste", "--no-newline"], &["xclip", "-selection", "clipboard", "-o"])?;
    let text = String::from_utf8(bytes).ok()?;
    (!text.is_empty()).then_some(text)
}

/// Read a PNG image from the desktop clipboard, if one is present.
pub fn read_image_png() -> Option<Vec<u8>> {
    // Prefer explicit image MIME types so we don't pull text as "image".
    if let Some(bytes) = read_clipboard_bytes(
        &["wl-paste", "--type", "image/png"],
        &["xclip", "-selection", "clipboard", "-t", "image/png", "-o"],
    ) {
        if looks_like_png(&bytes) {
            return Some(bytes);
        }
    }
    // Some compositors only expose image/jpeg or omit type filters.
    if let Some(bytes) = read_clipboard_bytes(
        &["wl-paste", "--type", "image/jpeg"],
        &["xclip", "-selection", "clipboard", "-t", "image/jpeg", "-o"],
    ) {
        if !bytes.is_empty() {
            return Some(bytes);
        }
    }
    None
}

/// Write `png` bytes to a unique temp file under the runtime dir and return
/// its absolute path. Caller is responsible for eventually cleaning up
/// (OS temp cleanup is fine for short-lived paste files).
pub fn write_paste_image(png_or_image: &[u8]) -> Option<PathBuf> {
    let dir = paste_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_nanos();
    // Use a real image extension: agents (Claude Code) detect attachments by
    // file extension, so a `.img` file would not be recognised as an image.
    // The only non-PNG source is the `image/jpeg` clipboard branch, so any
    // non-PNG payload here is JPEG.
    let ext = if looks_like_png(png_or_image) { "png" } else { "jpg" };
    let path = dir.join(format!("paste-{}-{}.{}", std::process::id(), nanos, ext));
    std::fs::write(&path, png_or_image).ok()?;
    Some(path)
}

/// If the clipboard holds an image, materialize it and return a paste
/// payload Claude Code / agents understand: `@/abs/path.png`.
/// Returns `None` when no image is available (caller should fall through
/// to normal Ctrl+V / text paste).
pub fn image_paste_payload() -> Option<String> {
    let bytes = read_image_png()?;
    let path = write_paste_image(&bytes)?;
    Some(format!("@{}", path.display()))
}

fn paste_dir() -> Option<PathBuf> {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime.is_empty() {
            return Some(Path::new(&runtime).join("cmux").join("pastes"));
        }
    }
    Some(std::env::temp_dir().join("cmux-pastes"))
}

fn looks_like_png(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

/// Try `wl_args` first (argv[0] = binary), then `x_args`.
fn read_clipboard_bytes(wl_args: &[&str], x_args: &[&str]) -> Option<Vec<u8>> {
    let from_wl = run_capture(wl_args);
    from_wl.or_else(|| run_capture(x_args))
}

fn run_capture(args: &[&str]) -> Option<Vec<u8>> {
    if args.is_empty() {
        return None;
    }
    let output = Command::new(args[0]).args(&args[1..]).output().ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_png_detects_signature() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        assert!(looks_like_png(&bytes));
        assert!(!looks_like_png(b"not a png"));
        assert!(!looks_like_png(b""));
    }

    #[test]
    fn write_paste_image_roundtrips() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(b"fake-png-body");
        let path = write_paste_image(&bytes).expect("write");
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn image_paste_payload_none_when_no_image_tool_or_empty() {
        // Without a real image on the clipboard this returns None; we only
        // assert it does not panic.
        let _ = image_paste_payload();
    }
}
