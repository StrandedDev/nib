//! The full-screen message shown instead of the editor when the file named
//! at startup cannot be edited.

use std::io::{self, Write};

use crossterm::{
    cursor::{Hide, MoveTo},
    event::{KeyCode, KeyEvent, KeyEventKind},
    queue,
    style::Print,
    terminal::{Clear, ClearType},
};
use unicode_width::UnicodeWidthStr;

use crate::render::{MarkColour, ResetFg, clean, fit_left};

const HINT: &str = "Press Enter to exit";

/// Draws `msg` centred on a cleared screen of size `(w, h)`, with a dim
/// hint below it.
pub(crate) fn draw(out: &mut impl Write, (w, h): (u16, u16), msg: &str) -> io::Result<()> {
    queue!(out, Hide, Clear(ClearType::All))?;
    if w == 0 || h == 0 {
        return out.flush();
    }
    let width = usize::from(w);
    let text = fit_left(&clean(msg), width, "\u{2026}");
    let y = h.saturating_sub(1) / 2;
    queue!(out, MoveTo(centre(&text, width), y), Print(&text))?;
    if y + 2 < h {
        let hint = fit_left(HINT, width, "\u{2026}");
        queue!(
            out,
            MoveTo(centre(&hint, width), y + 2),
            MarkColour,
            Print(&hint),
            ResetFg
        )?;
    }
    out.flush()
}

fn centre(s: &str, width: usize) -> u16 {
    u16::try_from(width.saturating_sub(s.width()) / 2).unwrap_or(0)
}

/// Whether `key` leaves the message screen: Enter, Space or Esc.
pub(crate) fn exits(key: &KeyEvent) -> bool {
    key.kind != KeyEventKind::Release
        && key.modifiers.is_empty()
        && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' '))
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};
    use tempfile::tempdir;

    use crate::app::App;
    use crate::app::test_support::*;

    #[test]
    fn hint_colour_reset_with_raw_sgr() {
        let mut out = Vec::new();
        super::draw(&mut out, (40, 5), "msg").unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("\u{1b}[90mPress Enter to exit\u{1b}[39m"),
            "{out:?}"
        );
    }

    #[test]
    fn binary_screen_exits_on_space() {
        let d = tempdir().unwrap();
        let p = d.path().join("b.bin");
        std::fs::write(&p, b"a\0b").unwrap();
        let mut app = App::open(Some(p.clone()), None).unwrap();
        assert_eq!(
            app.message.as_deref(),
            Some("b.bin is not a UTF-8 text file")
        );
        app.handle_event(key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(app.quit);
        assert_eq!(std::fs::read(&p).unwrap(), b"a\0b");
    }

    #[test]
    fn directory_screen_exits_on_enter_or_esc_only() {
        let d = tempdir().unwrap();
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        for exit in [KeyCode::Enter, KeyCode::Esc] {
            let mut app = App::open(Some(sub.clone()), None).unwrap();
            assert_eq!(app.message.as_deref(), Some("sub is a directory"));
            press(&mut app, 'x');
            app.handle_event(ctrl('s'));
            assert!(!app.quit && app.prompt.is_none());
            app.handle_event(key(exit, KeyModifiers::NONE));
            assert!(app.quit, "{exit:?}");
        }
        assert!(sub.is_dir());
        assert_eq!(std::fs::read_dir(&sub).unwrap().count(), 0);
    }
}
