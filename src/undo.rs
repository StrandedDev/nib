//! In-memory undo/redo history with typing grouped by word and pause.

use std::time::{Duration, Instant};

use crate::buffer::Buffer;
use crate::cursor::Pos;

const PAUSE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Deleting,
    Other,
}

/// One rope mutation: `removed` chars at `at` were replaced by `inserted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
    pub cursor_before: Pos,
    pub cursor_after: Pos,
}

struct Group {
    kind: EditKind,
    edits: Vec<Edit>,
}

#[derive(Default)]
pub struct UndoStack {
    groups: Vec<Group>,
    /// Groups currently applied; the rest are redo history.
    applied: usize,
    /// `applied` at the last save; `None` once redo history holding it is gone.
    saved: Option<usize>,
    last_at: Option<Instant>,
}

impl UndoStack {
    pub fn new() -> UndoStack {
        UndoStack {
            saved: Some(0),
            ..UndoStack::default()
        }
    }

    pub fn push(&mut self, edit: Edit, kind: EditKind, now: Instant) {
        self.groups.truncate(self.applied);
        if self.saved.is_some_and(|s| s > self.applied) {
            self.saved = None;
        }
        if self.can_merge(&edit, kind, now) {
            self.groups[self.applied - 1].edits.push(edit);
        } else {
            self.groups.push(Group {
                kind,
                edits: vec![edit],
            });
            self.applied = self.groups.len();
        }
        self.last_at = Some(now);
    }

    /// Adds `edit` to the group the last `push` made, so a multi-range
    /// change undoes as one step.
    pub fn push_joined(&mut self, edit: Edit, now: Instant) {
        match self.applied.checked_sub(1) {
            Some(i) => {
                self.groups[i].edits.push(edit);
                self.last_at = Some(now);
            }
            None => self.push(edit, EditKind::Other, now),
        }
    }

    fn can_merge(&self, edit: &Edit, kind: EditKind, now: Instant) -> bool {
        let Some(group) = self.applied.checked_sub(1).map(|i| &self.groups[i]) else {
            return false;
        };
        let (Some(last), Some(t)) = (group.edits.last(), self.last_at) else {
            return false;
        };
        if kind != group.kind || self.saved == Some(self.applied) || now.duration_since(t) >= PAUSE
        {
            return false;
        }
        match kind {
            EditKind::Typing => {
                let adjacent =
                    edit.removed.is_empty() && edit.at == last.at + last.inserted.chars().count();
                let closes_word = edit.inserted.starts_with(char::is_whitespace)
                    && last.inserted.ends_with(|c: char| !c.is_whitespace());
                adjacent && !closes_word
            }
            EditKind::Deleting => {
                let ends_at_last = edit.at + edit.removed.chars().count() == last.at;
                edit.inserted.is_empty()
                    && last.inserted.is_empty()
                    && (edit.at == last.at || ends_at_last)
            }
            EditKind::Other => false,
        }
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.applied);
    }

    /// No state in the history matches the file on disk.
    pub fn mark_unsaved(&mut self) {
        self.saved = None;
    }

    pub fn undo(&mut self, buf: &mut Buffer) -> Option<Pos> {
        if self.applied == 0 {
            return None;
        }
        self.applied -= 1;
        self.last_at = None;
        let group = &self.groups[self.applied];
        for e in group.edits.iter().rev() {
            splice(buf, e.at, &e.inserted, &e.removed);
        }
        buf.version += 1;
        buf.dirty = self.saved != Some(self.applied);
        Some(group.edits[0].cursor_before)
    }

    pub fn redo(&mut self, buf: &mut Buffer) -> Option<Pos> {
        let group = self.groups.get(self.applied)?;
        for e in &group.edits {
            splice(buf, e.at, &e.removed, &e.inserted);
        }
        self.applied += 1;
        self.last_at = None;
        buf.version += 1;
        buf.dirty = self.saved != Some(self.applied);
        Some(group.edits[group.edits.len() - 1].cursor_after)
    }
}

/// Replaces the chars of `old` at `at` with `new`.
fn splice(buf: &mut Buffer, at: usize, old: &str, new: &str) {
    let n = old.chars().count();
    if n > 0 {
        buf.rope.remove(at..at + n);
    }
    buf.rope.insert(at, new);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(col: usize) -> Pos {
        Pos { line: 0, col }
    }

    struct Rig {
        buf: Buffer,
        stack: UndoStack,
        t0: Instant,
    }

    impl Rig {
        fn new(s: &str) -> Rig {
            Rig {
                buf: Buffer::from_str_detect(s),
                stack: UndoStack::new(),
                t0: Instant::now(),
            }
        }

        fn text(&self) -> String {
            self.buf.rope.to_string()
        }

        /// Replace `len` chars at `at` with `ins`, `ms` after the start time.
        fn edit(&mut self, at: usize, len: usize, ins: &str, kind: EditKind, ms: u64) {
            let removed = self.buf.rope.slice(at..at + len).to_string();
            self.buf.rope.remove(at..at + len);
            self.buf.rope.insert(at, ins);
            self.buf.dirty = true;
            self.buf.version += 1;
            let edit = Edit {
                at,
                removed,
                inserted: ins.to_string(),
                cursor_before: pos(at + len),
                cursor_after: pos(at + ins.chars().count()),
            };
            let now = self.t0 + Duration::from_millis(ms);
            self.stack.push(edit, kind, now);
        }

        fn type_at(&mut self, at: usize, s: &str, ms: u64) {
            self.edit(at, 0, s, EditKind::Typing, ms);
        }
    }

    #[test]
    fn typing_word_is_one_undo() {
        let mut r = Rig::new("");
        for (i, c) in ["h", "e", "l", "l", "o"].iter().enumerate() {
            r.type_at(i, c, i as u64 * 100);
        }
        assert_eq!(r.text(), "hello");
        assert_eq!(r.stack.undo(&mut r.buf), Some(pos(0)));
        assert_eq!(r.text(), "");
        assert_eq!(r.stack.undo(&mut r.buf), None);
    }

    #[test]
    fn pause_splits_group() {
        let mut r = Rig::new("");
        r.type_at(0, "a", 0);
        r.type_at(1, "b", 999);
        r.type_at(2, "c", 2000);
        r.stack.undo(&mut r.buf);
        assert_eq!(r.text(), "ab");
        r.stack.undo(&mut r.buf);
        assert_eq!(r.text(), "");
    }

    #[test]
    fn space_splits_group() {
        let mut r = Rig::new("");
        r.type_at(0, "a", 0);
        r.type_at(1, "b", 10);
        r.type_at(2, " ", 20);
        r.type_at(3, "c", 30);
        r.stack.undo(&mut r.buf);
        assert_eq!(r.text(), "ab");
        r.stack.undo(&mut r.buf);
        assert_eq!(r.text(), "");
    }

    #[test]
    fn non_adjacent_typing_splits_group() {
        let mut r = Rig::new("");
        r.type_at(0, "a", 0);
        r.type_at(0, "b", 10);
        r.stack.undo(&mut r.buf);
        assert_eq!(r.text(), "a");
    }

    #[test]
    fn typing_over_selection_is_its_own_group() {
        let mut r = Rig::new("abc");
        r.type_at(3, "d", 0);
        r.edit(0, 4, "x", EditKind::Typing, 10);
        r.stack.undo(&mut r.buf);
        assert_eq!(r.text(), "abcd");
    }

    #[test]
    fn redo_after_undo() {
        let mut r = Rig::new("");
        r.type_at(0, "ab", 0);
        assert_eq!(r.stack.redo(&mut r.buf), None);
        r.stack.undo(&mut r.buf);
        assert_eq!(r.stack.redo(&mut r.buf), Some(pos(2)));
        assert_eq!(r.text(), "ab");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut r = Rig::new("");
        r.type_at(0, "ab", 0);
        r.stack.undo(&mut r.buf);
        r.type_at(0, "x", 5000);
        assert_eq!(r.stack.redo(&mut r.buf), None);
        assert_eq!(r.text(), "x");
    }

    #[test]
    fn new_edit_drops_redo_history_mid_stack() {
        let mut r = Rig::new("");
        r.type_at(0, "a", 0);
        r.type_at(1, "b", 5000);
        r.stack.undo(&mut r.buf);
        r.type_at(1, "c", 9000);
        r.stack.undo(&mut r.buf);
        assert_eq!(r.text(), "a");
        r.stack.undo(&mut r.buf);
        r.stack.redo(&mut r.buf);
        r.stack.redo(&mut r.buf);
        assert_eq!(r.text(), "ac");
        assert_eq!(r.stack.redo(&mut r.buf), None);
    }

    #[test]
    fn undo_to_save_point_clears_dirty() {
        let mut r = Rig::new("");
        r.type_at(0, "a", 0);
        r.stack.mark_saved();
        r.buf.dirty = false;
        r.type_at(1, "b", 10);
        r.stack.undo(&mut r.buf);
        assert_eq!(r.text(), "a");
        assert!(
            !r.buf.dirty,
            "typing after a save never joins the saved group"
        );
        r.stack.undo(&mut r.buf);
        assert!(r.buf.dirty);
        r.stack.redo(&mut r.buf);
        assert!(!r.buf.dirty);
    }

    #[test]
    fn redo_past_save_point_sets_dirty() {
        let mut r = Rig::new("");
        r.type_at(0, "a", 0);
        r.stack.mark_saved();
        r.stack.undo(&mut r.buf);
        r.type_at(0, "x", 5000);
        r.stack.undo(&mut r.buf);
        r.stack.redo(&mut r.buf);
        assert_eq!(r.text(), "x");
        assert!(r.buf.dirty, "the saved state is gone");
    }
}
