//! System clipboard, with OSC 52 as the fallback when there is no display.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// The OSC 52 "set clipboard" sequence for `s`.
pub fn osc52(s: &str) -> String {
    format!("\x1b]52;c;{}\x07", STANDARD.encode(s))
}

/// Longest base64 payload sent over OSC 52; some terminals drop larger ones.
const OSC52_MAX_B64: usize = 100 * 1024;

pub const TOO_LARGE: &str = "Copied (too large for terminal clipboard)";

pub trait Clip {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, s: &str);

    /// Terminal output the caller must write after the frame, if any.
    fn take_pending(&mut self) -> Option<String> {
        None
    }

    /// A status message for the last `set`, if it needs one.
    fn take_notice(&mut self) -> Option<&'static str> {
        None
    }
}

/// OSC 52 through the terminal. Never writes by itself: the sequence waits
/// in `pending` until the app has flushed a frame.
#[derive(Default)]
pub struct Osc52 {
    last: Option<String>,
    pending: Option<String>,
    notice: Option<&'static str>,
}

impl Clip for Osc52 {
    fn get(&mut self) -> Option<String> {
        self.last.clone()
    }

    fn set(&mut self, s: &str) {
        self.last = Some(s.to_string());
        if s.len().div_ceil(3) * 4 > OSC52_MAX_B64 {
            self.pending = None;
            self.notice = Some(TOO_LARGE);
        } else {
            self.pending = Some(osc52(s));
        }
    }

    fn take_pending(&mut self) -> Option<String> {
        self.pending.take()
    }

    fn take_notice(&mut self) -> Option<&'static str> {
        self.notice.take()
    }
}

/// The OS clipboard, with an in-process copy for when reading it fails.
pub struct System {
    board: arboard::Clipboard,
    last: Option<String>,
}

impl Clip for System {
    fn get(&mut self) -> Option<String> {
        self.board.get_text().ok().or_else(|| self.last.clone())
    }

    fn set(&mut self, s: &str) {
        self.last = Some(s.to_string());
        let _ = self.board.set_text(s);
    }
}

/// System clipboard on Windows, or on Linux with a display it can open;
/// OSC 52 otherwise.
pub fn detect() -> Box<dyn Clip> {
    let has_display = cfg!(windows)
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    match has_display.then(arboard::Clipboard::new) {
        Some(Ok(board)) => Box::new(System { board, last: None }),
        _ => Box::new(Osc52::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_sequence_format() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn osc52_pending_taken_once() {
        let mut c = Osc52::default();
        c.set("hi");
        assert_eq!(c.take_pending().as_deref(), Some("\x1b]52;c;aGk=\x07"));
        assert_eq!(c.take_pending(), None);
        assert_eq!(c.get().as_deref(), Some("hi"));
    }

    #[test]
    fn osc52_large_payload_skipped() {
        let mut c = Osc52::default();
        let big = "a".repeat(80_000);
        c.set(&big);
        assert_eq!(c.take_pending(), None);
        assert_eq!(c.take_notice(), Some(TOO_LARGE));
        assert_eq!(c.get().as_deref(), Some(big.as_str()));
        c.set("small");
        assert!(c.take_pending().is_some());
        assert_eq!(c.take_notice(), None);
    }
}
