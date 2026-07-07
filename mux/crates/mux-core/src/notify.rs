//! Watches raw pty output for OSC desktop-notification sequences (OSC 9,
//! OSC 777, and the kitty notification protocol all parse into the same
//! command via `ghostty_vt::OscParser`), independent of and in parallel
//! with the authoritative VT parser in [`crate::surface`].
//!
//! This is a best-effort side channel, not a second terminal parser: it
//! only tracks enough state to recognize an OSC sequence's start (`ESC ]`)
//! and end (`BEL` or `ST`), and hands everything in between to
//! libghostty-vt's own OSC parser. A missed edge case here can't corrupt
//! or desync actual terminal rendering, since that's driven entirely by
//! the separate `term.vt_write()` call on the same bytes.

use ghostty_vt::OscParser;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    SawEsc,
    InOsc,
    InOscSawEsc,
}

pub struct OscWatcher {
    parser: OscParser,
    state: State,
}

impl OscWatcher {
    pub fn new() -> anyhow::Result<Self> {
        Ok(OscWatcher {
            parser: OscParser::new().map_err(|e| anyhow::anyhow!("{e:?}"))?,
            state: State::Idle,
        })
    }

    /// Scans a chunk of raw pty output, returning every desktop
    /// notification (title, body) whose terminating byte fell within it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for &byte in bytes {
            match self.state {
                State::Idle => {
                    if byte == 0x1b {
                        self.state = State::SawEsc;
                    }
                }
                State::SawEsc => {
                    self.state = if byte == b']' {
                        self.parser.reset();
                        State::InOsc
                    } else {
                        State::Idle
                    };
                }
                State::InOsc => match byte {
                    0x07 => {
                        if let Some(n) = self.parser.end(0x07).desktop_notification() {
                            found.push(n);
                        }
                        self.state = State::Idle;
                    }
                    0x1b => self.state = State::InOscSawEsc,
                    _ => self.parser.next(byte),
                },
                State::InOscSawEsc => {
                    if byte == b'\\' {
                        if let Some(n) = self.parser.end(0x5c).desktop_notification() {
                            found.push(n);
                        }
                    }
                    // Anything else after an ESC mid-OSC isn't a valid ST;
                    // abandon this sequence rather than misparse it.
                    self.state = State::Idle;
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bel_terminated_osc9() {
        let mut watcher = OscWatcher::new().unwrap();
        let found = watcher.feed(b"before \x1b]9;Build failed\x07 after");
        assert_eq!(found, vec![("".to_string(), "Build failed".to_string())]);
    }

    #[test]
    fn detects_st_terminated_osc777_with_title() {
        let mut watcher = OscWatcher::new().unwrap();
        let found = watcher.feed(b"\x1b]777;notify;Tests;3 failed\x1b\\");
        assert_eq!(found, vec![("Tests".to_string(), "3 failed".to_string())]);
    }

    #[test]
    fn ignores_non_notification_osc_sequences() {
        let mut watcher = OscWatcher::new().unwrap();
        // OSC 0: change window title - not a notification.
        let found = watcher.feed(b"\x1b]0;my shell\x07");
        assert!(found.is_empty());
    }

    #[test]
    fn sequence_split_across_multiple_feed_calls() {
        let mut watcher = OscWatcher::new().unwrap();
        assert!(watcher.feed(b"\x1b]9;Hello ").is_empty());
        let found = watcher.feed(b"world\x07");
        assert_eq!(found, vec![("".to_string(), "Hello world".to_string())]);
    }

    #[test]
    fn unrelated_escape_sequences_do_not_confuse_the_scanner() {
        let mut watcher = OscWatcher::new().unwrap();
        // A CSI sequence (cursor move) followed by a real notification.
        let found = watcher.feed(b"\x1b[2J\x1b]9;after clear\x07");
        assert_eq!(found, vec![("".to_string(), "after clear".to_string())]);
    }

    #[test]
    fn multiple_notifications_in_one_chunk() {
        let mut watcher = OscWatcher::new().unwrap();
        let found = watcher.feed(b"\x1b]9;first\x07 middle \x1b]9;second\x07");
        assert_eq!(
            found,
            vec![("".to_string(), "first".to_string()), ("".to_string(), "second".to_string())]
        );
    }
}
