//! Incremental find and per-match replace.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ropey::Rope;

use crate::cursor::{self, Pos};
use crate::edit::Editor;
use crate::prompt::{Prompt, PromptResult};

/// A prepared query. Smart case: insensitive unless the query has an
/// uppercase char. Folding is per char and only for chars whose lowercase is
/// a single char, so char offsets stay 1:1 with the text.
struct Pattern {
    chars: Vec<char>,
    fold: bool,
}

fn fold_char(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

impl Pattern {
    fn new(query: &str) -> Option<Pattern> {
        if query.is_empty() {
            return None;
        }
        let fold = !query.chars().any(char::is_uppercase);
        let chars = query
            .chars()
            .map(|c| if fold { fold_char(c) } else { c })
            .collect();
        Some(Pattern { chars, fold })
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    fn matches_at(&self, line: &[char], i: usize) -> bool {
        line.get(i..i + self.len()).is_some_and(|w| {
            w.iter().zip(&self.chars).all(
                |(&t, &p)| {
                    if self.fold { fold_char(t) == p } else { t == p }
                },
            )
        })
    }

    /// First (or last) match start in `lo..hi` of `line`.
    fn scan(&self, line: &[char], lo: usize, hi: usize, forward: bool) -> Option<usize> {
        let hi = hi.min((line.len() + 1).checked_sub(self.len())?);
        if forward {
            (lo..hi).find(|&i| self.matches_at(line, i))
        } else {
            (lo..hi).rev().find(|&i| self.matches_at(line, i))
        }
    }
}

fn line_chars(rope: &Rope, line: usize) -> Vec<char> {
    rope.line(line).chars().collect()
}

/// The next match from `from` (a start at or after it going forward, before
/// it going back), scanning line by line and stopping at the first hit. With
/// `wrap` the search continues from the other end; the flag says it did.
/// Matches never span a newline because the query is a single line.
pub fn find_next(
    rope: &Rope,
    query: &str,
    from: usize,
    forward: bool,
    wrap: bool,
) -> Option<(Range<usize>, bool)> {
    let pat = Pattern::new(query)?;
    let from = from.min(rope.len_chars());
    let line0 = rope.char_to_line(from);
    let col0 = from - rope.line_to_char(line0);
    let lines = rope.len_lines();
    // The lines to visit, and which starts count on each: the first pass
    // covers `from` onwards, the wrapped pass what that pass skipped.
    let order = |first_pass: bool| -> Box<dyn Iterator<Item = usize>> {
        match (forward, first_pass) {
            (true, true) => Box::new(line0..lines),
            (true, false) => Box::new(0..=line0),
            (false, true) => Box::new((0..=line0).rev()),
            (false, false) => Box::new((line0..lines).rev()),
        }
    };
    let bounds = |l: usize, first_pass: bool| {
        if l != line0 {
            (0, usize::MAX)
        } else if forward == first_pass {
            (col0, usize::MAX)
        } else {
            (0, col0)
        }
    };
    let run = |first_pass: bool| {
        order(first_pass).find_map(|l| {
            let (lo, hi) = bounds(l, first_pass);
            let c = pat.scan(&line_chars(rope, l), lo, hi, forward)?;
            let at = rope.line_to_char(l) + c;
            Some((at..at + pat.len(), !first_pass))
        })
    };
    run(true).or_else(|| if wrap { run(false) } else { None })
}

/// Non-overlapping matches that overlap `range`, in order. Reads only the
/// lines touching `range`, and on each only the part near it, so the cost
/// follows what is on screen.
pub fn matches_in(rope: &Rope, query: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let Some(pat) = Pattern::new(query) else {
        return Vec::new();
    };
    let len = rope.len_chars();
    let (start, end) = (range.start.min(len), range.end.min(len));
    let pad = pat.len() - 1;
    let mut out = Vec::new();
    for l in rope.char_to_line(start)..=rope.char_to_line(end) {
        let base = rope.line_to_char(l);
        let line_end = base + rope.line(l).len_chars();
        let a = start.saturating_sub(pad).max(base);
        let b = (end + pad).min(line_end);
        let chars: Vec<char> = rope.slice(a..b).chars().collect();
        let mut i = 0;
        while let Some(c) = pat.scan(&chars, i, usize::MAX, true) {
            i = c + pat.len();
            let m = a + c..a + i;
            if m.end > start && m.start < end {
                out.push(m);
            }
        }
    }
    out
}

/// Every match in the buffer. For replace-all only; searches and highlights
/// use the bounded functions above.
pub fn find_all(rope: &Rope, query: &str) -> Vec<Range<usize>> {
    matches_in(rope, query, 0..rope.len_chars())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Find,
    With,
    Choose,
}

/// What the app should do after a key reached the session.
#[derive(Default)]
pub struct FindOutcome {
    /// The session is over; the app drops it.
    pub close: bool,
    /// Also run the key as a normal command (it was a bound command).
    pub forward: bool,
    /// The buffer changed.
    pub edited: bool,
    pub status: Option<String>,
}

/// The Find and Replace interaction. The app routes keys here while it is
/// active; the session moves the editor's selection onto matches.
pub struct FindSession {
    replace: bool,
    stage: Stage,
    prompt: Prompt,
    query: String,
    /// Cursor and anchor when the session opened, restored on Esc.
    origin: (Pos, Option<Pos>),
    /// Char index of the origin cursor: where searches start.
    start: usize,
    /// The current match; it is also the editor's selection.
    cur: Option<Range<usize>>,
    replacement: String,
    replaced: usize,
    /// Replace cycle: the char index where it ends once it has wrapped.
    limit: usize,
    wrapped: bool,
}

impl FindSession {
    pub fn new(ed: &Editor, replace: bool) -> FindSession {
        FindSession {
            replace,
            stage: Stage::Find,
            prompt: Prompt::text("Find: "),
            query: String::new(),
            origin: (ed.cursor, ed.anchor),
            start: cursor::to_char_idx(&ed.buf.rope, ed.cursor),
            cur: None,
            replacement: String::new(),
            replaced: 0,
            limit: 0,
            wrapped: false,
        }
    }

    pub fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    /// Matches to underline in the visible char range.
    pub fn visible_matches(&self, rope: &Rope, range: Range<usize>) -> Vec<Range<usize>> {
        matches_in(rope, &self.query, range)
    }

    fn input(&self) -> &str {
        match &self.prompt {
            Prompt::Text { input, .. } => input,
            Prompt::Choice { .. } => "",
        }
    }

    /// `bound` says the keymap has a command for the key.
    pub fn handle_key(&mut self, key: &KeyEvent, bound: bool, ed: &mut Editor) -> FindOutcome {
        let mut out = FindOutcome::default();
        if key.kind == KeyEventKind::Release {
            return out;
        }
        if self.stage == Stage::Choose {
            return self.choose(key, ed);
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let own = key.code == KeyCode::Esc
            || (plain && matches!(key.code, KeyCode::Enter | KeyCode::Backspace));
        if bound && !own {
            // Arrows, Ctrl+S and the like end the prompt, keeping the match.
            out.close = true;
            out.forward = true;
            return out;
        }
        // Some legacy terminals send Shift+Enter as plain Enter, so previous
        // match may only be reachable through the palette there.
        let shift_enter = key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT);
        if shift_enter && self.stage == Stage::Find && !self.replace {
            self.step(false, ed, &mut out);
            return out;
        }
        match self.prompt.handle_key(key) {
            PromptResult::Cancel => self.finish(ed, &mut out),
            PromptResult::Submit(text) => self.submit(text, ed, &mut out),
            _ => self.query_changed(ed, &mut out),
        }
        out
    }

    pub fn paste(&mut self, s: &str, ed: &mut Editor) -> FindOutcome {
        let mut out = FindOutcome::default();
        if self.stage != Stage::Choose {
            self.prompt.paste(s);
            self.query_changed(ed, &mut out);
        }
        out
    }

    /// Reruns the search from the origin when the query text changed.
    fn query_changed(&mut self, ed: &mut Editor, out: &mut FindOutcome) {
        if self.stage != Stage::Find || self.input() == self.query {
            return;
        }
        self.query = self.input().to_string();
        let found = find_next(&ed.buf.rope, &self.query, self.start, true, true);
        match found {
            Some((m, _)) => self.select(m, ed),
            None => {
                ed.move_to(self.origin.0, false);
                self.cur = None;
                if !self.query.is_empty() {
                    out.status = Some("Not found".into());
                }
            }
        }
    }

    fn select(&mut self, m: Range<usize>, ed: &mut Editor) {
        let rope = &ed.buf.rope;
        let (a, b) = (
            cursor::from_char_idx(rope, m.start),
            cursor::from_char_idx(rope, m.end),
        );
        ed.move_to(a, false);
        ed.move_to(b, true);
        self.cur = Some(m);
    }

    fn restore(&self, ed: &mut Editor) {
        ed.move_to(self.origin.0, false);
        ed.anchor = self.origin.1;
    }

    fn submit(&mut self, text: String, ed: &mut Editor, out: &mut FindOutcome) {
        match (self.stage, self.replace) {
            (Stage::Find, false) => self.step(true, ed, out),
            (Stage::Find, true) if self.query.is_empty() => self.finish(ed, out),
            (Stage::Find, true) => {
                self.stage = Stage::With;
                self.prompt = Prompt::text("Replace with: ");
            }
            _ => {
                self.replacement = text;
                self.stage = Stage::Choose;
                self.prompt = Prompt::Choice {
                    label: "Replace? (R)eplace / (A)ll / (S)kip / Esc".into(),
                    keys: &['r', 'a', 's'],
                };
                (self.limit, self.wrapped) = (self.start, false);
                if find_next(&ed.buf.rope, &self.query, self.start, true, true).is_none() {
                    self.restore(ed);
                    out.close = true;
                    out.status = Some("Not found".into());
                } else {
                    self.advance(self.start, ed, out);
                }
            }
        }
    }

    /// Enter / Shift+Enter: the next or previous match, wrapping.
    fn step(&mut self, forward: bool, ed: &mut Editor, out: &mut FindOutcome) {
        let Some(cur) = self.cur.clone() else {
            return;
        };
        let from = if forward { cur.end } else { cur.start };
        if let Some((m, wrapped)) = find_next(&ed.buf.rope, &self.query, from, forward, true) {
            self.select(m, ed);
            if wrapped {
                out.status = Some("Search wrapped".into());
            }
        }
    }

    /// Replace cycle: selects the next match from `from`, or ends the
    /// session once the search is back where the cycle began.
    fn advance(&mut self, from: usize, ed: &mut Editor, out: &mut FindOutcome) {
        let found = find_next(&ed.buf.rope, &self.query, from, true, true);
        match found {
            Some((m, w))
                if !(w && self.wrapped) && !((w || self.wrapped) && m.start >= self.limit) =>
            {
                self.wrapped |= w;
                self.select(m, ed);
            }
            _ => self.finish(ed, out),
        }
    }

    /// Ends the session. Nothing replaced: the cursor and selection come
    /// back; otherwise they stay and the status counts the replacements.
    fn finish(&mut self, ed: &mut Editor, out: &mut FindOutcome) {
        out.close = true;
        if self.replaced == 0 {
            self.restore(ed);
        } else {
            ed.anchor = None;
            out.status = Some(format!("Replaced {}", self.replaced));
        }
    }

    fn choose(&mut self, key: &KeyEvent, ed: &mut Editor) -> FindOutcome {
        let mut out = FindOutcome::default();
        let Some(cur) = self.cur.clone() else {
            return out;
        };
        match self.prompt.handle_key(key) {
            PromptResult::Choice('s') => self.advance(cur.end, ed, &mut out),
            PromptResult::Choice('r') => {
                ed.replace_many(std::slice::from_ref(&cur), &self.replacement);
                out.edited = true;
                self.replaced += 1;
                let new_len = self.replacement.chars().count();
                if self.wrapped {
                    self.limit = (self.limit + new_len).saturating_sub(cur.len());
                }
                self.advance(cur.start + new_len, ed, &mut out);
            }
            PromptResult::Choice('a') => {
                // Matches the cycle has not reached yet.
                let rest: Vec<Range<usize>> = find_all(&ed.buf.rope, &self.query)
                    .into_iter()
                    .filter(|m| {
                        let ahead = m.start >= cur.start;
                        if self.wrapped {
                            ahead && m.start < self.limit
                        } else {
                            ahead || m.start < self.limit
                        }
                    })
                    .collect();
                ed.replace_many(&rest, &self.replacement);
                out.edited = true;
                self.replaced += rest.len();
                self.finish(ed, &mut out);
            }
            PromptResult::Cancel => self.finish(ed, &mut out),
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use crate::cursor::Pos;
    use crate::edit::Editor;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn ed(s: &str) -> Editor {
        Editor::new(Buffer::from_str_detect(s))
    }

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    fn press(
        f: &mut FindSession,
        e: &mut Editor,
        code: KeyCode,
        mods: KeyModifiers,
    ) -> FindOutcome {
        f.handle_key(&KeyEvent::new(code, mods), false, e)
    }

    fn typ(f: &mut FindSession, e: &mut Editor, s: &str) {
        for c in s.chars() {
            press(f, e, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    fn enter(f: &mut FindSession, e: &mut Editor) -> FindOutcome {
        press(f, e, KeyCode::Enter, KeyModifiers::NONE)
    }

    #[test]
    fn smart_case_lower_insensitive() {
        let r = Rope::from_str("Foo foo FOO");
        assert_eq!(find_all(&r, "foo"), vec![0..3, 4..7, 8..11]);
    }

    #[test]
    fn smart_case_upper_sensitive() {
        let r = Rope::from_str("Foo foo FOO");
        assert_eq!(find_all(&r, "Foo"), vec![0..3]);
        assert_eq!(find_all(&r, "fOO"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn multibyte_match_ranges() {
        let r = Rope::from_str("é foo\n你 foo");
        assert_eq!(find_all(&r, "foo"), vec![2..5, 8..11]);
        let (m, wrapped) = find_next(&r, "foo", 3, true, true).unwrap();
        assert_eq!((m, wrapped), (8..11, false));
    }

    #[test]
    fn dotted_i_does_not_shift_offsets() {
        // 'İ' lowercases to two chars, so it only matches itself.
        let r = Rope::from_str("İi İ");
        assert_eq!(find_all(&r, "i"), vec![1..2]);
        assert_eq!(find_all(&r, "İ"), vec![0..1, 3..4]);
        assert_eq!(find_all(&r, "ii"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn next_wraps() {
        let r = Rope::from_str("ab\nxab\nab");
        assert_eq!(find_next(&r, "ab", 0, true, true), Some((0..2, false)));
        assert_eq!(find_next(&r, "ab", 2, true, true), Some((4..6, false)));
        assert_eq!(find_next(&r, "ab", 8, true, true), Some((0..2, true)));
        assert_eq!(find_next(&r, "ab", 8, true, false), None);
        assert_eq!(find_next(&r, "ab", 4, false, true), Some((0..2, false)));
        assert_eq!(find_next(&r, "ab", 0, false, true), Some((7..9, true)));
        assert_eq!(find_next(&r, "ab", 0, false, false), None);
        assert_eq!(find_next(&r, "", 0, true, true), None);
    }

    #[test]
    fn matches_in_limits_to_range() {
        let r = Rope::from_str("ab ab\nab ab\nab");
        assert_eq!(matches_in(&r, "ab", 6..9), vec![6..8]);
        // A match straddling the start of the range is included.
        assert_eq!(matches_in(&r, "ab", 1..3), vec![0..2]);
    }

    #[test]
    fn find_esc_restores_cursor_and_selection() {
        let mut e = ed("one two
three two");
        e.move_to(pos(0, 1), false);
        e.move_to(pos(0, 3), true);
        let mut f = FindSession::new(&e, false);
        typ(&mut f, &mut e, "two");
        assert_eq!(e.selection(), Some(4..7));
        let out = press(&mut f, &mut e, KeyCode::Esc, KeyModifiers::NONE);
        assert!(out.close);
        assert_eq!((e.cursor, e.anchor), (pos(0, 3), Some(pos(0, 1))));
    }

    #[test]
    fn find_not_found_status() {
        let mut e = ed("one two");
        e.move_to(pos(0, 2), false);
        let mut f = FindSession::new(&e, false);
        typ(&mut f, &mut e, "tw");
        assert_eq!(e.selection(), Some(4..6));
        let out = press(&mut f, &mut e, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(out.status.as_deref(), Some("Not found"));
        assert_eq!((e.cursor, e.anchor), (pos(0, 2), None));
        // Backspacing back to a match selects it again.
        press(&mut f, &mut e, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(e.selection(), Some(4..6));
    }

    #[test]
    fn find_enter_and_shift_enter_step_and_wrap() {
        let mut e = ed("ab ab ab");
        let mut f = FindSession::new(&e, false);
        typ(&mut f, &mut e, "ab");
        assert_eq!(e.selection(), Some(0..2));
        assert_eq!(enter(&mut f, &mut e).status, None);
        assert_eq!(e.selection(), Some(3..5));
        enter(&mut f, &mut e);
        let out = enter(&mut f, &mut e);
        assert_eq!(out.status.as_deref(), Some("Search wrapped"));
        assert_eq!(e.selection(), Some(0..2));
        let out = press(&mut f, &mut e, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(out.status.as_deref(), Some("Search wrapped"));
        assert_eq!(e.selection(), Some(6..8));
        press(&mut f, &mut e, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(e.selection(), Some(3..5));
    }

    #[test]
    fn find_other_command_keeps_match() {
        let mut e = ed("one two");
        let mut f = FindSession::new(&e, false);
        typ(&mut f, &mut e, "two");
        let out = f.handle_key(
            &KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            true,
            &mut e,
        );
        assert!(out.close && out.forward);
        assert_eq!(e.selection(), Some(4..7));
    }

    fn replace_session(e: &mut Editor, find: &str, with: &str) -> FindSession {
        let mut f = FindSession::new(e, true);
        typ(&mut f, e, find);
        enter(&mut f, e);
        typ(&mut f, e, with);
        enter(&mut f, e);
        f
    }

    fn choice(f: &mut FindSession, e: &mut Editor, c: char) -> FindOutcome {
        press(f, e, KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn skip_leaves_match() {
        let mut e = ed("ab ab ab");
        let mut f = replace_session(&mut e, "ab", "X");
        assert!(
            matches!(f.prompt(), Prompt::Choice { label, .. } if label == "Replace? (R)eplace / (A)ll / (S)kip / Esc")
        );
        assert_eq!(e.selection(), Some(0..2));
        choice(&mut f, &mut e, 's');
        assert_eq!(e.buf.rope.to_string(), "ab ab ab");
        assert_eq!(e.selection(), Some(3..5));
        let out = choice(&mut f, &mut e, 'r');
        assert!(out.edited && !out.close);
        assert_eq!(e.buf.rope.to_string(), "ab X ab");
        assert_eq!(e.selection(), Some(5..7));
        let out = choice(&mut f, &mut e, 's');
        assert!(out.close);
        assert_eq!(out.status.as_deref(), Some("Replaced 1"));
    }

    #[test]
    fn replace_all_one_undo() {
        let mut e = ed("ab ab
ab");
        let mut f = replace_session(&mut e, "ab", "Z");
        choice(&mut f, &mut e, 's');
        let out = choice(&mut f, &mut e, 'a');
        assert!(out.close && out.edited);
        // The skipped first match is replaced too: the cycle wraps to it.
        assert_eq!(
            e.buf.rope.to_string(),
            "ab Z
Z"
        );
        e.undo();
        assert_eq!(
            e.buf.rope.to_string(),
            "ab ab
ab"
        );
        e.redo();
        assert_eq!(
            e.buf.rope.to_string(),
            "ab Z
Z"
        );
    }

    #[test]
    fn replace_all_status_count() {
        let mut e = ed("ab ab ab");
        let mut f = replace_session(&mut e, "ab", "");
        choice(&mut f, &mut e, 'r');
        let out = choice(&mut f, &mut e, 'a');
        assert_eq!(e.buf.rope.to_string(), "  ");
        assert_eq!(out.status.as_deref(), Some("Replaced 3"));
        assert!(!e.buf.rope.to_string().contains("ab"));
    }

    #[test]
    fn replace_all_covers_matches_before_the_cursor() {
        let mut e = ed("ab ab ab");
        e.move_to(pos(0, 4), false);
        let mut f = replace_session(&mut e, "ab", "Z");
        assert_eq!(e.selection(), Some(6..8));
        choice(&mut f, &mut e, 'r');
        // Wrapped to the matches before the start; the one under it starts
        // before the cursor, so it is part of the cycle.
        assert_eq!(e.selection(), Some(0..2));
        choice(&mut f, &mut e, 'r');
        assert_eq!(e.selection(), Some(2..4));
        let out = choice(&mut f, &mut e, 's');
        assert!(out.close);
        assert_eq!(e.buf.rope.to_string(), "Z ab Z");
    }

    #[test]
    fn replace_text_containing_query_terminates() {
        let mut e = ed("aa");
        let mut f = replace_session(&mut e, "a", "aa");
        let mut presses = 0;
        loop {
            let out = choice(&mut f, &mut e, 'r');
            presses += 1;
            assert!(presses < 20, "did not terminate");
            if out.close {
                break;
            }
        }
        assert_eq!(e.buf.rope.to_string(), "aaaa");
        // The same with a wrapped start and Replace all.
        let mut e = ed("xa a");
        e.move_to(pos(0, 3), false);
        let mut f = replace_session(&mut e, "a", "aa");
        let out = choice(&mut f, &mut e, 'a');
        assert!(out.close);
        assert_eq!(e.buf.rope.to_string(), "xaa aa");
    }

    #[test]
    fn replace_esc_keeps_earlier_replacements() {
        let mut e = ed("ab ab");
        let mut f = replace_session(&mut e, "ab", "Z");
        choice(&mut f, &mut e, 'r');
        let out = press(&mut f, &mut e, KeyCode::Esc, KeyModifiers::NONE);
        assert!(out.close);
        assert_eq!(e.buf.rope.to_string(), "Z ab");
        assert_eq!(out.status.as_deref(), Some("Replaced 1"));
        assert_eq!(e.anchor, None);
    }

    #[test]
    fn replace_no_match_closes_with_not_found() {
        let mut e = ed("abc");
        e.move_to(pos(0, 1), false);
        let mut f = FindSession::new(&e, true);
        typ(&mut f, &mut e, "zz");
        enter(&mut f, &mut e);
        let out = enter(&mut f, &mut e);
        assert!(out.close && !out.edited);
        assert_eq!(out.status.as_deref(), Some("Not found"));
        assert_eq!((e.cursor, e.anchor), (pos(0, 1), None));
    }

    #[test]
    fn find_not_found_clears_existing_selection() {
        let mut e = ed("one two");
        e.move_to(pos(0, 1), false);
        e.move_to(pos(0, 3), true);
        let mut f = FindSession::new(&e, false);
        typ(&mut f, &mut e, "zz");
        assert_eq!((e.cursor, e.anchor), (pos(0, 3), None));
    }

    #[test]
    fn replace_ctrl_a_does_not_replace_all() {
        let mut e = ed("ab ab");
        let mut f = replace_session(&mut e, "ab", "Z");
        let out = press(&mut f, &mut e, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(!out.close && !out.edited);
        assert_eq!(e.buf.rope.to_string(), "ab ab");
    }
}
