//! Safe wrapper around libghostty-vt's standalone OSC parser.
//!
//! This is independent of [`crate::Terminal`]: it's the same parser Ghostty
//! itself uses to recognize OSC (Operating System Command) sequences, but
//! exposed as a small streaming state machine an embedder can drive with
//! whatever bytes it's already scanning for OSC framing (`ESC ]` ... `BEL`
//! or `ESC \`). Feed the *payload* bytes only (everything between the
//! opener and the terminator) with [`OscParser::next`], then call
//! [`OscParser::end`] with the terminator byte to get the parsed command.

use std::ffi::CStr;

use ghostty_vt_sys as sys;

use crate::{check, Result};

/// A streaming OSC sequence parser.
pub struct OscParser {
    raw: sys::GhosttyOscParser,
}

// SAFETY: the C API has no thread-affinity requirements documented; the
// parser only touches memory it owns via its allocator.
unsafe impl Send for OscParser {}

impl OscParser {
    pub fn new() -> Result<Self> {
        let mut raw: sys::GhosttyOscParser = std::ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer for the lifetime of this call.
        check(unsafe { sys::ghostty_osc_new(std::ptr::null(), &mut raw) })?;
        Ok(OscParser { raw })
    }

    /// Feeds one payload byte (not the `ESC ]` opener or the terminator).
    pub fn next(&mut self, byte: u8) {
        // SAFETY: `self.raw` is a live parser handle for the life of `self`.
        unsafe { sys::ghostty_osc_next(self.raw, byte) };
    }

    /// Finalizes the sequence. `terminator` is `0x07` (BEL) or `0x5c` (`\`,
    /// the second byte of an ST/`ESC \` terminator) — whichever byte ended
    /// the sequence in the source stream.
    pub fn end(&mut self, terminator: u8) -> OscCommand<'_> {
        // SAFETY: `self.raw` is live; the returned handle borrows from it
        // (invalidated by the next `ghostty_osc_*` call on this parser,
        // which the `'_` lifetime ties to `&mut self`).
        let raw = unsafe { sys::ghostty_osc_end(self.raw, terminator) };
        OscCommand { raw, _parser: std::marker::PhantomData }
    }

    /// Resets parser state, discarding any partially parsed sequence.
    pub fn reset(&mut self) {
        // SAFETY: `self.raw` is a live parser handle.
        unsafe { sys::ghostty_osc_reset(self.raw) };
    }
}

impl Drop for OscParser {
    fn drop(&mut self) {
        // SAFETY: `self.raw` was created by `ghostty_osc_new` and is freed
        // exactly once, here.
        unsafe { sys::ghostty_osc_free(self.raw) };
    }
}

/// A parsed OSC command, borrowed from the [`OscParser`] that produced it.
pub struct OscCommand<'a> {
    raw: sys::GhosttyOscCommand,
    _parser: std::marker::PhantomData<&'a mut OscParser>,
}

impl OscCommand<'_> {
    /// The desktop notification's title and body (OSC 9, OSC 777, or the
    /// kitty notification protocol all parse into this one command type;
    /// title is often empty). `None` if this command isn't a
    /// `show_desktop_notification`.
    pub fn desktop_notification(&self) -> Option<(String, String)> {
        let command_type = unsafe { sys::ghostty_osc_command_type(self.raw) };
        if command_type != sys::GHOSTTY_OSC_COMMAND_SHOW_DESKTOP_NOTIFICATION {
            return None;
        }
        let title = self.extract_str(sys::GHOSTTY_OSC_DATA_DESKTOP_NOTIFICATION_TITLE_STR)?;
        let body = self.extract_str(sys::GHOSTTY_OSC_DATA_DESKTOP_NOTIFICATION_BODY_STR)?;
        Some((title, body))
    }

    fn extract_str(&self, data: sys::GhosttyOscCommandData) -> Option<String> {
        let mut out: *const std::os::raw::c_char = std::ptr::null();
        // SAFETY: `self.raw` is a valid command handle for the lifetime of
        // `self`; `out` is a valid out-pointer of the type this data kind
        // documents (`const char **`).
        let ok = unsafe {
            sys::ghostty_osc_command_data(
                self.raw,
                data,
                &mut out as *mut _ as *mut std::os::raw::c_void,
            )
        };
        if !ok || out.is_null() {
            return None;
        }
        // SAFETY: `ok` guarantees `out` was set to a valid, null-terminated
        // string owned by the parser, live until the next `ghostty_osc_*`
        // call — which we make none of before copying it out here.
        Some(unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(parser: &mut OscParser, payload: &str) {
        for byte in payload.bytes() {
            parser.next(byte);
        }
    }

    #[test]
    fn parses_osc9_desktop_notification() {
        let mut parser = OscParser::new().unwrap();
        feed(&mut parser, "9;Build failed");
        let command = parser.end(0x07);
        let (title, body) = command.desktop_notification().unwrap();
        assert_eq!(title, "");
        assert_eq!(body, "Build failed");
    }

    #[test]
    fn parses_osc777_rxvt_notification_with_title() {
        let mut parser = OscParser::new().unwrap();
        feed(&mut parser, "777;notify;Tests;3 failed");
        let command = parser.end(0x07);
        let (title, body) = command.desktop_notification().unwrap();
        assert_eq!(title, "Tests");
        assert_eq!(body, "3 failed");
    }

    #[test]
    fn non_notification_command_returns_none() {
        let mut parser = OscParser::new().unwrap();
        feed(&mut parser, "0;window title");
        let command = parser.end(0x07);
        assert!(command.desktop_notification().is_none());
    }

    #[test]
    fn reset_discards_partial_sequence() {
        let mut parser = OscParser::new().unwrap();
        feed(&mut parser, "9;partial");
        parser.reset();
        feed(&mut parser, "9;fresh");
        let command = parser.end(0x07);
        let (_, body) = command.desktop_notification().unwrap();
        assert_eq!(body, "fresh");
    }
}
