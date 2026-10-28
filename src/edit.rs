//! Text mutations over a buffer and cursor.

use std::ops::Range;
use std::time::Instant;

use crate::buffer::Buffer;
use crate::cursor::{self, Pos};
use crate::indent::{self, Enter, Indent};
use crate::layout;
use crate::undo::{Edit, EditKind, UndoStack};

pub struct Editor {
    pub buf: Buffer,
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    pub preferred_col: Option<usize>,
    /// What Tab inserts; detected when the file loads.
    pub indent: Indent,
    undo: UndoStack,
    now: Option<Instant>,
}

impl Editor {
    pub fn new(buf: Buffer) -> Editor {
        Editor {
            buf,
            cursor: Pos::default(),
            anchor: None,
            preferred_col: None,
            indent: Indent::Spaces(4),
            undo: UndoStack::new(),
            now: None,
        }
    }

    /// Pins the clock used for undo grouping; `None` uses the real one.
    #[cfg(test)]
    pub(crate) fn set_now(&mut self, now: Option<Instant>) {
        self.now = now;
    }

    pub fn mark_saved(&mut self) {
        self.undo.mark_saved();
    }

    /// The text differs from the file and no undo gets back to it.
    pub fn mark_unsaved(&mut self) {
        self.undo.mark_unsaved();
        self.buf.dirty = true;
    }

    pub fn undo(&mut self) {
        let pos = self.undo.undo(&mut self.buf);
        self.settle(pos);
    }

    pub fn redo(&mut self) {
        let pos = self.undo.redo(&mut self.buf);
        self.settle(pos);
    }

    fn settle(&mut self, pos: Option<Pos>) {
        if let Some(p) = pos {
            self.cursor = cursor::clamp(&self.buf.rope, p);
            self.anchor = None;
            self.preferred_col = None;
        }
    }

    /// The selected char range, ordered; `None` when there is no anchor or it
    /// sits on the cursor.
    pub fn selection(&self) -> Option<Range<usize>> {
        let rope = &self.buf.rope;
        let a = cursor::to_char_idx(rope, self.anchor?);
        let c = cursor::to_char_idx(rope, self.cursor);
        (a != c).then(|| a.min(c)..a.max(c))
    }

    /// The chars a line cut removes: the cursor line with its newline. The
    /// last line has none, so it takes the preceding newline instead.
    pub fn cut_line_range(&self) -> Range<usize> {
        let rope = &self.buf.rope;
        let line = self.cursor.line.min(rope.len_lines() - 1);
        let start = rope.line_to_char(line);
        if line + 1 < rope.len_lines() {
            start..rope.line_to_char(line + 1)
        } else {
            start.saturating_sub(1)..rope.len_chars()
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        Some(self.buf.rope.slice(self.selection()?).to_string())
    }

    /// Removes and returns the selection, or the cursor line when there is
    /// none (always ending in `\n`). One mutation.
    pub fn cut(&mut self) -> String {
        if let Some(text) = self.selected_text() {
            self.delete_selection();
            return text;
        }
        let range = self.cut_line_range();
        let mut text = self.buf.rope.slice(range.clone()).to_string();
        let line = self.cursor.line;
        self.replace_range(range, "", EditKind::Other);
        if text.starts_with('\n') {
            text.remove(0);
        }
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let last = self.buf.rope.len_lines() - 1;
        self.move_to(
            Pos {
                line: line.min(last),
                col: 0,
            },
            false,
        );
        text
    }

    pub fn select_all(&mut self) {
        self.move_to(cursor::file_start(&self.buf.rope), false);
        self.move_to(cursor::file_end(&self.buf.rope), true);
    }

    /// Drops the selection, leaving the cursor at its start (`to_start`) or end.
    pub fn collapse(&mut self, to_start: bool) {
        let Some(r) = self.selection() else {
            return;
        };
        let at = if to_start { r.start } else { r.end };
        self.move_to(cursor::from_char_idx(&self.buf.rope, at), false);
    }

    /// Selects the word, punctuation or whitespace run under `pos`.
    pub fn select_word(&mut self, pos: Pos) {
        let (start, end) = cursor::word_at(&self.buf.rope, pos);
        self.move_to(start, false);
        self.move_to(end, true);
    }

    /// Keyboard text input; consecutive typing groups into one undo step.
    pub fn type_text(&mut self, s: &str) {
        self.insert(s, EditKind::Typing);
    }

    pub fn insert_str(&mut self, s: &str) {
        self.insert(s, EditKind::Other);
    }

    fn insert(&mut self, s: &str, kind: EditKind) {
        if s.is_empty() {
            return;
        }
        let s = s.replace("\r\n", "\n");
        let range = self.selection().unwrap_or_else(|| {
            let at = cursor::to_char_idx(&self.buf.rope, self.cursor);
            at..at
        });
        self.replace_range(range, &s, kind);
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = cursor::left(&self.buf.rope, self.cursor);
        self.delete_range(start, self.cursor, EditKind::Deleting);
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let end = cursor::right(&self.buf.rope, self.cursor);
        self.delete_range(self.cursor, end, EditKind::Deleting);
    }

    /// Tab: with no selection, pads to the next stop of the indent width (or
    /// a real tab); with one, replaces it with a single indent unit.
    pub fn tab(&mut self) {
        let unit = match self.indent {
            Indent::Tabs => "\t".to_string(),
            Indent::Spaces(n) => {
                let n = usize::from(n.max(1));
                let width = if self.selection().is_some() {
                    0
                } else {
                    layout::display_col(&self.buf.rope, self.cursor, n as u8)
                };
                " ".repeat(n - width % n)
            }
        };
        self.type_text(&unit);
    }

    /// Enter: one undo step. Indent and list rules use the line where the
    /// selection (or cursor) starts.
    pub fn newline(&mut self) {
        let rope = &self.buf.rope;
        let range = self.selection().unwrap_or_else(|| {
            let at = cursor::to_char_idx(rope, self.cursor);
            at..at
        });
        let line = rope.char_to_line(range.start);
        let start = rope.line_to_char(line);
        let text = rope.line(line).to_string();
        let text = text.strip_suffix('\n').unwrap_or(&text);
        match indent::plan_enter(text, range.start - start, self.selection().is_some()) {
            Enter::Insert(s) => self.replace_range(range, &s, EditKind::Other),
            Enter::ClearLine => {
                let end = start + text.chars().count();
                self.replace_range(start..end, "", EditKind::Other);
            }
        }
    }

    pub fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = cursor::word_left(&self.buf.rope, self.cursor);
        self.delete_range(start, self.cursor, EditKind::Other);
    }

    pub fn delete_word_fwd(&mut self) {
        if self.delete_selection() {
            return;
        }
        let end = cursor::word_right(&self.buf.rope, self.cursor);
        self.delete_range(self.cursor, end, EditKind::Other);
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

    /// Removes the selection, if any. Returns whether there was one.
    fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some(r) => {
                self.replace_range(r, "", EditKind::Other);
                true
            }
            None => false,
        }
    }

    /// Removes `[start, end)` and puts the cursor at `start`; empty ranges are no-ops.
    fn delete_range(&mut self, start: Pos, end: Pos, kind: EditKind) {
        let a = cursor::to_char_idx(&self.buf.rope, start);
        let b = cursor::to_char_idx(&self.buf.rope, end);
        self.replace_range(a..b, "", kind);
    }

    /// The one mutation path: replaces chars `range` with `s`, leaves the
    /// cursor after `s`, drops the anchor, and records the change for undo.
    /// Empty-for-empty is a no-op.
    fn replace_range(&mut self, range: Range<usize>, s: &str, kind: EditKind) {
        if let Some(edit) = self.apply(range, s) {
            let now = self.now.unwrap_or_else(Instant::now);
            self.undo.push(edit, kind, now);
        }
    }

    /// Replaces each of `ranges` (ascending, not overlapping) with `s` as
    /// one undo step. Applied back to front so earlier offsets stay valid.
    pub fn replace_many(&mut self, ranges: &[Range<usize>], s: &str) {
        let mut last = true;
        for r in ranges.iter().rev() {
            if last {
                self.replace_range(r.clone(), s, EditKind::Other);
                last = false;
            } else if let Some(edit) = self.apply(r.clone(), s) {
                let now = self.now.unwrap_or_else(Instant::now);
                self.undo.push_joined(edit, now);
            }
        }
    }

    fn apply(&mut self, range: Range<usize>, s: &str) -> Option<Edit> {
        if range.start >= range.end && s.is_empty() {
            return None;
        }
        let at = range.start;
        let cursor_before = self.cursor;
        let removed = self.buf.rope.slice(range.clone()).to_string();
        if range.start < range.end {
            self.buf.rope.remove(range);
        }
        self.buf.rope.insert(at, s);
        self.buf.dirty = true;
        self.buf.version += 1;
        self.cursor = cursor::from_char_idx(&self.buf.rope, at + s.chars().count());
        self.anchor = None;
        self.preferred_col = None;
        Some(Edit {
            at,
            removed,
            inserted: s.to_string(),
            cursor_before,
            cursor_after: self.cursor,
        })
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

    #[test]
    fn selection_is_ordered_char_range_and_empty_is_none() {
        let mut e = ed("ab
cd");
        assert_eq!(e.selection(), None);
        e.move_to(pos(1, 1), false);
        e.move_to(pos(0, 1), true);
        assert_eq!(e.selection(), Some(1..4));
        e.move_to(pos(1, 1), false);
        e.move_to(pos(1, 1), true);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn typing_replaces_selection() {
        let mut e = ed("hello world");
        e.move_to(pos(0, 6), false);
        e.move_to(pos(0, 11), true);
        e.insert_str("rust");
        assert_eq!(text(&e), "hello rust");
        assert_eq!(e.cursor, pos(0, 10));
        assert_eq!(e.anchor, None);
        assert_eq!(e.buf.version, 1, "one mutation");
    }

    #[test]
    fn edit_clears_anchor() {
        let ops: [fn(&mut Editor); 5] = [
            |e| e.insert_str("x"),
            |e| e.backspace(),
            |e| e.delete(),
            |e| e.delete_word_back(),
            |e| e.delete_word_fwd(),
        ];
        for op in ops {
            let mut e = ed("foo bar baz");
            e.move_to(pos(0, 4), false);
            e.move_to(pos(0, 7), true);
            op(&mut e);
            assert_eq!(e.anchor, None);
            assert!(text(&e).starts_with("foo "), "{}", text(&e));
            assert!(text(&e).ends_with(" baz"), "{}", text(&e));
            assert_eq!(e.cursor, pos(0, if text(&e).len() == 8 { 4 } else { 5 }));
        }
        // An anchor left past the end of a shrunken buffer is clamped.
        let mut e = ed("abc");
        e.anchor = Some(pos(5, 5));
        assert_eq!(e.selection(), Some(0..3));
        e.cursor = pos(0, 3);
        e.insert_str("d");
        assert_eq!(e.anchor, None);
    }

    #[test]
    fn deleting_selection_across_lines() {
        let mut e = ed("ab\ncd\nef");
        e.move_to(pos(0, 1), false);
        e.move_to(pos(2, 1), true);
        e.backspace();
        assert_eq!(text(&e), "af");
        assert_eq!(e.cursor, pos(0, 1));
        assert_eq!(e.buf.version, 1);
    }

    #[test]
    fn select_all_covers_buffer() {
        let mut e = ed("ab\ncd");
        e.select_all();
        assert_eq!(e.selection(), Some(0..5));
        assert_eq!((e.anchor, e.cursor), (Some(pos(0, 0)), pos(1, 2)));
        let mut e = ed("");
        e.select_all();
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn collapse_goes_to_selection_edges() {
        let mut e = ed("abcd");
        e.move_to(pos(0, 3), false);
        e.move_to(pos(0, 1), true);
        e.collapse(false);
        assert_eq!((e.cursor, e.anchor), (pos(0, 3), None));
        e.move_to(pos(0, 3), true);
        e.move_to(pos(0, 1), true);
        e.collapse(true);
        assert_eq!((e.cursor, e.anchor), (pos(0, 1), None));
    }

    #[test]
    fn cut_line_range_middle_line_includes_newline() {
        let mut e = ed("ab\ncd\nef");
        e.move_to(pos(1, 1), false);
        assert_eq!(e.cut_line_range(), 3..6);
    }

    #[test]
    fn cut_last_line_takes_preceding_newline() {
        let mut e = ed("ab\ncd\nef");
        e.move_to(pos(2, 1), false);
        assert_eq!(e.cut(), "ef\n");
        assert_eq!(text(&e), "ab\ncd");
        assert_eq!(e.cursor, pos(1, 0));
    }

    #[test]
    fn cut_only_line_leaves_empty_buffer() {
        let mut e = ed("abc");
        assert_eq!(e.cut(), "abc\n");
        assert_eq!(text(&e), "");
        assert_eq!(e.cursor, pos(0, 0));
    }

    #[test]
    fn cut_middle_line_and_selection() {
        let mut e = ed("ab\ncd\nef");
        e.move_to(pos(1, 1), false);
        assert_eq!(e.cut(), "cd\n");
        assert_eq!(text(&e), "ab\nef");
        assert_eq!(e.cursor, pos(1, 0));
        assert_eq!(e.buf.version, 1);
        e.move_to(pos(0, 0), false);
        e.move_to(pos(1, 1), true);
        assert_eq!(e.cut(), "ab\ne");
        assert_eq!(text(&e), "f");
        assert_eq!(e.buf.version, 2);
    }

    #[test]
    fn undo_restores_selection_replaced_text() {
        let mut e = ed("abc");
        e.move_to(pos(0, 0), false);
        e.move_to(pos(0, 3), true);
        e.type_text("x");
        assert_eq!(text(&e), "x");
        e.undo();
        assert_eq!(text(&e), "abc");
        assert_eq!((e.cursor, e.anchor), (pos(0, 3), None));
        e.redo();
        assert_eq!(text(&e), "x");
        assert_eq!(e.cursor, pos(0, 1));
    }

    #[test]
    fn consecutive_backspaces_merge() {
        let mut e = ed("abcdef");
        e.set_now(Some(Instant::now()));
        e.move_to(pos(0, 6), false);
        for _ in 0..3 {
            e.backspace();
        }
        assert_eq!(text(&e), "abc");
        e.undo();
        assert_eq!(text(&e), "abcdef");
        assert_eq!(e.cursor, pos(0, 6));
    }

    #[test]
    fn typing_and_backspace_do_not_merge() {
        let mut e = ed("");
        e.set_now(Some(Instant::now()));
        e.type_text("a");
        e.type_text("b");
        e.backspace();
        e.undo();
        assert_eq!(text(&e), "ab");
        e.undo();
        assert_eq!(text(&e), "");
    }

    #[test]
    fn cut_is_one_step() {
        let mut e = ed("ab
cd
ef");
        e.move_to(pos(1, 1), false);
        e.cut();
        e.undo();
        assert_eq!(
            text(&e),
            "ab
cd
ef"
        );
        assert_eq!(e.cursor, pos(1, 1));
        e.insert_str(
            "1
2",
        );
        e.undo();
        assert_eq!(
            text(&e),
            "ab
cd
ef"
        );
    }

    #[test]
    fn undo_bumps_version() {
        let mut e = ed("");
        e.set_now(Some(Instant::now()));
        e.type_text("a");
        e.type_text("b");
        assert_eq!(e.buf.version, 2);
        e.undo();
        assert_eq!(e.buf.version, 3);
        e.redo();
        assert_eq!(e.buf.version, 4);
    }

    #[test]
    fn undo_empty_is_noop() {
        let mut e = ed("ab");
        e.move_to(pos(0, 1), false);
        e.undo();
        e.redo();
        assert_eq!(
            (e.buf.version, e.buf.dirty, e.cursor),
            (0, false, pos(0, 1))
        );
        assert_eq!(text(&e), "ab");
    }

    #[test]
    fn undo_resets_preferred_col_and_anchor() {
        let mut e = ed("abc");
        e.type_text("x");
        e.preferred_col = Some(4);
        e.move_to(pos(0, 0), true);
        e.preferred_col = Some(4);
        e.undo();
        assert_eq!((e.anchor, e.preferred_col), (None, None));
    }

    #[test]
    fn enter_copies_indent() {
        let mut e = ed("  \tab");
        e.move_to(pos(0, 5), false);
        e.newline();
        assert_eq!(text(&e), "  \tab\n  \t");
        assert_eq!(e.cursor, pos(1, 3));
    }

    #[test]
    fn enter_inside_indent_does_not_duplicate() {
        let mut e = ed("    x");
        e.move_to(pos(0, 2), false);
        e.newline();
        assert_eq!(text(&e), "  \n    x");
        assert_eq!(e.cursor, pos(1, 2));
    }

    #[test]
    fn enter_continues_dash() {
        let mut e = ed("  - item");
        e.move_to(pos(0, 8), false);
        e.newline();
        assert_eq!(text(&e), "  - item\n  - ");
        assert_eq!(e.cursor, pos(1, 4));
    }

    fn enter_at_end(src: &str) -> Editor {
        let mut e = ed(src);
        e.move_to(cursor::file_end(&e.buf.rope), false);
        e.newline();
        e
    }

    #[test]
    fn enter_increments_number() {
        let e = enter_at_end("9. x");
        assert_eq!(text(&e), "9. x\n10. ");
        assert_eq!(e.cursor, pos(1, 4));
        assert_eq!(text(&enter_at_end("  007. x")), "  007. x\n  8. ");
    }

    #[test]
    fn enter_resets_checkbox() {
        let e = enter_at_end("- [x] done");
        assert_eq!(text(&e), "- [x] done\n- [ ] ");
        assert_eq!(e.cursor, pos(1, 6));
    }

    #[test]
    fn enter_on_star_and_x_checkbox() {
        assert_eq!(text(&enter_at_end("* [ ] a")), "* [ ] a\n* [ ] ");
        assert_eq!(text(&enter_at_end("* [X] a")), "* [X] a\n* [ ] ");
        assert_eq!(text(&enter_at_end("* a")), "* a\n* ");
    }

    #[test]
    fn enter_on_empty_item_ends_list() {
        let e = enter_at_end("- a\n  - [ ] ");
        assert_eq!(text(&e), "- a\n");
        assert_eq!(e.cursor, pos(1, 0));
        assert_eq!(text(&enter_at_end("1. a\n2.   ")), "1. a\n");
    }

    #[test]
    fn enter_empty_item_is_one_undo() {
        let mut e = enter_at_end("- a\n- ");
        assert_eq!(text(&e), "- a\n");
        e.undo();
        assert_eq!(text(&e), "- a\n- ");
        assert_eq!(e.cursor, pos(1, 2));
    }

    #[test]
    fn enter_inside_marker_is_plain() {
        let mut e = ed("  - x");
        e.move_to(pos(0, 3), false);
        e.newline();
        assert_eq!(text(&e), "  -\n   x");
        assert_eq!(e.cursor, pos(1, 2));
    }

    #[test]
    fn tab_aligns_to_next_stop_after_cjk() {
        let mut e = ed("\u{4e2d}");
        e.move_to(pos(0, 1), false);
        e.tab();
        assert_eq!(text(&e), "\u{4e2d}  ");
        e.indent = Indent::Spaces(2);
        e.tab();
        assert_eq!(text(&e), "\u{4e2d}    ");
    }

    #[test]
    fn tab_after_tab_char_and_tabs_mode() {
        let mut e = ed("\tx");
        e.move_to(pos(0, 2), false);
        e.tab();
        assert_eq!(text(&e), "\tx   ");
        e.indent = Indent::Tabs;
        e.tab();
        assert_eq!(text(&e), "\tx   \t");
    }

    #[test]
    fn tab_with_selection_replaces_it() {
        let mut e = ed("abcdef");
        e.move_to(pos(0, 1), false);
        e.move_to(pos(0, 4), true);
        e.tab();
        assert_eq!(text(&e), "a    ef");
        assert_eq!(e.cursor, pos(0, 5));
    }

    #[test]
    fn replace_many_is_one_undo_step() {
        let mut e = ed("a b a b a");
        e.replace_many(&[0..1, 4..5, 8..9], "xyz");
        assert_eq!(text(&e), "xyz b xyz b xyz");
        e.undo();
        assert_eq!(text(&e), "a b a b a");
        e.redo();
        assert_eq!(text(&e), "xyz b xyz b xyz");
        e.undo();
        assert_eq!(text(&e), "a b a b a");
        assert!(!e.buf.dirty);
    }
}
