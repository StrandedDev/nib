//! Text mutations over a buffer and cursor.

use crate::buffer::Buffer;
use crate::cursor::{self, Pos};

pub struct Editor {
    pub buf: Buffer,
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    pub preferred_col: Option<usize>,
}

impl Editor {
    pub fn new(buf: Buffer) -> Editor {
        Editor {
            buf,
            cursor: Pos::default(),
            anchor: None,
            preferred_col: None,
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let s = s.replace("\r\n", "\n");
        let at = cursor::to_char_idx(&self.buf.rope, self.cursor);
        self.buf.rope.insert(at, &s);
        self.buf.dirty = true;
        self.buf.version += 1;
        self.cursor = cursor::from_char_idx(&self.buf.rope, at + s.chars().count());
        self.preferred_col = None;
    }

    pub fn backspace(&mut self) {
        let start = cursor::left(&self.buf.rope, self.cursor);
        self.delete_range(start, self.cursor);
    }

    pub fn delete(&mut self) {
        let end = cursor::right(&self.buf.rope, self.cursor);
        self.delete_range(self.cursor, end);
    }

    pub fn newline(&mut self) {
        self.insert_str("\n");
    }

    pub fn delete_word_back(&mut self) {
        let start = cursor::word_left(&self.buf.rope, self.cursor);
        self.delete_range(start, self.cursor);
    }

    pub fn delete_word_fwd(&mut self) {
        let end = cursor::word_right(&self.buf.rope, self.cursor);
        self.delete_range(self.cursor, end);
    }

    pub fn move_to(&mut self, pos: Pos, select: bool) {
        let pos = cursor::clamp(&self.buf.rope, pos);
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
        self.preferred_col = None;
    }

    /// Removes `[start, end)` and puts the cursor at `start`; empty ranges are no-ops.
    fn delete_range(&mut self, start: Pos, end: Pos) {
        let a = cursor::to_char_idx(&self.buf.rope, start);
        let b = cursor::to_char_idx(&self.buf.rope, end);
        if a >= b {
            return;
        }
        self.buf.rope.remove(a..b);
        self.buf.dirty = true;
        self.buf.version += 1;
        self.cursor = cursor::from_char_idx(&self.buf.rope, a);
        self.preferred_col = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(s: &str) -> Editor {
        Editor::new(Buffer::from_str_detect(s))
    }

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    fn text(e: &Editor) -> String {
        e.buf.rope.to_string()
    }

    #[test]
    fn insert_then_backspace_restores() {
        let mut e = ed("ab");
        e.move_to(pos(0, 1), false);
        e.insert_str("xy\nz");
        assert_eq!(text(&e), "axy\nzb");
        assert_eq!(e.cursor, pos(1, 1));
        assert!(e.buf.dirty);
        for _ in 0..4 {
            e.backspace();
        }
        assert_eq!(text(&e), "ab");
        assert_eq!(e.cursor, pos(0, 1));
    }

    #[test]
    fn insert_normalises_crlf_keeps_lone_cr() {
        let mut e = ed("");
        e.insert_str("a\r\nb\rc");
        assert_eq!(text(&e), "a\nb\rc");
        assert_eq!(e.cursor, pos(1, 3));
    }

    #[test]
    fn backspace_joins_lines() {
        let mut e = ed("ab\ncd");
        e.move_to(pos(1, 0), false);
        e.backspace();
        assert_eq!(text(&e), "abcd");
        assert_eq!(e.cursor, pos(0, 2));
    }

    #[test]
    fn backspace_at_file_start_noop() {
        let mut e = ed("ab");
        e.backspace();
        assert_eq!(text(&e), "ab");
        assert!(!e.buf.dirty);
    }

    #[test]
    fn delete_removes_grapheme_and_joins() {
        let mut e = ed("ab\ncd");
        e.delete();
        assert_eq!(text(&e), "b\ncd");
        e.move_to(pos(0, 1), false);
        e.delete();
        assert_eq!(text(&e), "bcd");
        assert_eq!(e.cursor, pos(0, 1));
    }

    #[test]
    fn delete_at_eof_noop() {
        let mut e = ed("ab");
        e.move_to(pos(0, 2), false);
        e.delete();
        assert_eq!(text(&e), "ab");
        assert_eq!(e.cursor, pos(0, 2));
        assert!(!e.buf.dirty);
    }

    #[test]
    fn backspace_removes_whole_grapheme() {
        let mut e = ed("e\u{301}");
        e.move_to(pos(0, 2), false);
        e.backspace();
        assert_eq!(text(&e), "");
        assert_eq!(e.cursor, pos(0, 0));
    }

    #[test]
    fn ctrl_backspace_deletes_word() {
        let mut e = ed("foo bar baz");
        e.move_to(pos(0, 7), false);
        e.delete_word_back();
        assert_eq!(text(&e), "foo  baz");
        assert_eq!(e.cursor, pos(0, 4));
    }

    #[test]
    fn delete_word_fwd_deletes_word() {
        let mut e = ed("foo bar baz");
        e.move_to(pos(0, 3), false);
        e.delete_word_fwd();
        assert_eq!(text(&e), "foo baz");
        assert_eq!(e.cursor, pos(0, 3));
    }

    #[test]
    fn newline_splits_line() {
        let mut e = ed("abcd");
        e.move_to(pos(0, 2), false);
        e.newline();
        assert_eq!(text(&e), "ab\ncd");
        assert_eq!(e.cursor, pos(1, 0));
    }

    #[test]
    fn move_to_selection_anchor_and_clamp() {
        let mut e = ed("ab\ncd");
        e.move_to(pos(0, 1), true);
        assert_eq!(e.anchor, Some(pos(0, 0)));
        e.move_to(pos(9, 9), true);
        assert_eq!(e.anchor, Some(pos(0, 0)));
        assert_eq!(e.cursor, pos(1, 2));
        e.move_to(pos(0, 0), false);
        assert_eq!(e.anchor, None);
        assert!(!e.buf.dirty);
    }

    #[test]
    fn horizontal_edit_resets_preferred_col() {
        let mut e = ed("abc");
        e.preferred_col = Some(5);
        e.insert_str("x");
        assert_eq!(e.preferred_col, None);
        e.preferred_col = Some(5);
        e.move_to(pos(0, 0), false);
        assert_eq!(e.preferred_col, None);
    }

    #[test]
    fn real_edits_bump_buffer_version() {
        let mut e = ed("ab");
        assert_eq!(e.buf.version, 0);
        e.insert_str("");
        e.cursor = pos(0, 2);
        e.delete();
        assert_eq!(e.buf.version, 0, "no-ops do not bump");
        e.insert_str("x");
        assert_eq!(e.buf.version, 1);
        e.backspace();
        assert_eq!(e.buf.version, 2);
    }
}
