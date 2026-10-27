//! Pure cursor motion over a rope, in grapheme clusters.
//!
//! `col` is a char index within the line, excluding the trailing `\n`.
//! Segmentation runs over rope chunks of the single line, so the cost of a
//! step is proportional to the grapheme, not the line.

use ropey::{Rope, RopeSlice};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn classify(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn last_line(rope: &Rope) -> usize {
    rope.len_lines() - 1
}

/// The line's text without its trailing `\n`.
pub(crate) fn line_text(rope: &Rope, line: usize) -> RopeSlice<'_> {
    let s = rope.line(line);
    let n = s.len_chars();
    if n > 0 && s.char(n - 1) == '\n' {
        s.slice(..n - 1)
    } else {
        s
    }
}

fn line_len(rope: &Rope, line: usize) -> usize {
    line_text(rope, line).len_chars()
}

fn next_boundary(s: RopeSlice, byte: usize) -> usize {
    let len = s.len_bytes();
    let mut gc = GraphemeCursor::new(byte, len, true);
    let (mut chunk, mut chunk_start, _, _) = s.chunk_at_byte(byte);
    loop {
        match gc.next_boundary(chunk, chunk_start) {
            Ok(None) => return len,
            Ok(Some(n)) => return n,
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_start += chunk.len();
                chunk = s.chunk_at_byte(chunk_start).0;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (ctx, ctx_start, _, _) = s.chunk_at_byte(n - 1);
                gc.provide_context(ctx, ctx_start);
            }
            Err(_) => return len,
        }
    }
}

fn prev_boundary(s: RopeSlice, byte: usize) -> usize {
    let mut gc = GraphemeCursor::new(byte, s.len_bytes(), true);
    let (mut chunk, mut chunk_start, _, _) = s.chunk_at_byte(byte.saturating_sub(1));
    loop {
        match gc.prev_boundary(chunk, chunk_start) {
            Ok(None) => return 0,
            Ok(Some(n)) => return n,
            Err(GraphemeIncomplete::PrevChunk) => {
                (chunk, chunk_start, _, _) = s.chunk_at_byte(chunk_start - 1);
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let (ctx, ctx_start, _, _) = s.chunk_at_byte(n - 1);
                gc.provide_context(ctx, ctx_start);
            }
            Err(_) => return 0,
        }
    }
}

pub(crate) fn next_col(s: RopeSlice, col: usize) -> usize {
    s.byte_to_char(next_boundary(s, s.char_to_byte(col)))
}

fn prev_col(s: RopeSlice, col: usize) -> usize {
    s.byte_to_char(prev_boundary(s, s.char_to_byte(col)))
}

/// Start of the grapheme containing `col`; `len` when at or past the end.
fn snap(s: RopeSlice, col: usize) -> usize {
    let len = s.len_chars();
    if col >= len {
        len
    } else {
        prev_col(s, next_col(s, col))
    }
}

pub fn clamp(rope: &Rope, p: Pos) -> Pos {
    let line = p.line.min(last_line(rope));
    Pos {
        line,
        col: snap(line_text(rope, line), p.col),
    }
}

pub fn left(rope: &Rope, p: Pos) -> Pos {
    let p = clamp(rope, p);
    if p.col > 0 {
        Pos {
            line: p.line,
            col: prev_col(line_text(rope, p.line), p.col),
        }
    } else if p.line > 0 {
        Pos {
            line: p.line - 1,
            col: line_len(rope, p.line - 1),
        }
    } else {
        p
    }
}

pub fn right(rope: &Rope, p: Pos) -> Pos {
    let p = clamp(rope, p);
    let s = line_text(rope, p.line);
    if p.col < s.len_chars() {
        Pos {
            line: p.line,
            col: next_col(s, p.col),
        }
    } else if p.line < last_line(rope) {
        Pos {
            line: p.line + 1,
            col: 0,
        }
    } else {
        p
    }
}

pub fn word_left(rope: &Rope, p: Pos) -> Pos {
    let p = clamp(rope, p);
    if p.col == 0 {
        return left(rope, p);
    }
    let s = line_text(rope, p.line);
    let mut col = p.col;
    while col > 0 {
        let pc = prev_col(s, col);
        if classify(s.char(pc)) != Class::Space {
            break;
        }
        col = pc;
    }
    if col > 0 {
        let class = classify(s.char(prev_col(s, col)));
        while col > 0 {
            let pc = prev_col(s, col);
            if classify(s.char(pc)) != class {
                break;
            }
            col = pc;
        }
    }
    Pos { line: p.line, col }
}

pub fn word_right(rope: &Rope, p: Pos) -> Pos {
    let p = clamp(rope, p);
    let s = line_text(rope, p.line);
    let len = s.len_chars();
    if p.col >= len {
        return right(rope, p);
    }
    let mut col = p.col;
    while col < len && classify(s.char(col)) == Class::Space {
        col = next_col(s, col);
    }
    if col < len {
        let class = classify(s.char(col));
        while col < len && classify(s.char(col)) == class {
            col = next_col(s, col);
        }
    }
    Pos { line: p.line, col }
}

pub fn line_start(_rope: &Rope, p: Pos) -> Pos {
    Pos {
        line: p.line,
        col: 0,
    }
}

pub fn line_end(rope: &Rope, p: Pos) -> Pos {
    let line = p.line.min(last_line(rope));
    Pos {
        line,
        col: line_len(rope, line),
    }
}

pub fn file_start(_rope: &Rope) -> Pos {
    Pos::default()
}

pub fn file_end(rope: &Rope) -> Pos {
    line_end(
        rope,
        Pos {
            line: last_line(rope),
            col: 0,
        },
    )
}

pub fn to_char_idx(rope: &Rope, p: Pos) -> usize {
    let p = clamp(rope, p);
    rope.line_to_char(p.line) + p.col
}

pub fn from_char_idx(rope: &Rope, idx: usize) -> Pos {
    let idx = idx.min(rope.len_chars());
    let line = rope.char_to_line(idx);
    Pos {
        line,
        col: idx - rope.line_to_char(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn right_steps_over_zwj_emoji() {
        let r = Rope::from_str("👨‍👩‍👧x");
        assert_eq!(right(&r, pos(0, 0)), pos(0, 5));
        assert_eq!(left(&r, pos(0, 5)), pos(0, 0));
    }

    #[test]
    fn right_at_eol_goes_next_line() {
        let r = Rope::from_str("ab\ncd");
        assert_eq!(right(&r, pos(0, 2)), pos(1, 0));
        assert_eq!(right(&r, pos(1, 2)), pos(1, 2));
    }

    #[test]
    fn left_at_bol_goes_prev_eol() {
        let r = Rope::from_str("ab\ncd");
        assert_eq!(left(&r, pos(1, 0)), pos(0, 2));
        assert_eq!(left(&r, pos(0, 0)), pos(0, 0));
    }

    #[test]
    fn word_right_skips_space_then_word() {
        let r = Rope::from_str(
            "  foo_1 bar
x",
        );
        assert_eq!(word_right(&r, pos(0, 0)), pos(0, 7));
        assert_eq!(word_right(&r, pos(0, 7)), pos(0, 11));
        assert_eq!(word_right(&r, pos(0, 11)), pos(1, 0));
    }

    #[test]
    fn word_left_over_punct() {
        let r = Rope::from_str(
            "foo ==  bar
x",
        );
        assert_eq!(word_left(&r, pos(0, 11)), pos(0, 8));
        assert_eq!(word_left(&r, pos(0, 8)), pos(0, 4));
        assert_eq!(word_left(&r, pos(0, 4)), pos(0, 0));
        assert_eq!(word_left(&r, pos(1, 0)), pos(0, 11));
    }

    #[test]
    fn line_and_file_bounds() {
        let r = Rope::from_str(
            "ab
cde
",
        );
        assert_eq!(line_start(&r, pos(1, 2)), pos(1, 0));
        assert_eq!(line_end(&r, pos(1, 0)), pos(1, 3));
        assert_eq!(file_start(&r), pos(0, 0));
        assert_eq!(file_end(&r), pos(2, 0));
    }

    #[test]
    fn clamp_limits_and_snaps_to_grapheme_start() {
        let r = Rope::from_str(
            "👨‍👩‍👧x
ab",
        );
        assert_eq!(clamp(&r, pos(0, 3)), pos(0, 0));
        assert_eq!(clamp(&r, pos(0, 5)), pos(0, 5));
        assert_eq!(clamp(&r, pos(0, 99)), pos(0, 6));
        assert_eq!(clamp(&r, pos(9, 99)), pos(1, 2));
    }

    #[test]
    fn char_idx_round_trip() {
        let r = Rope::from_str(
            "ab
cde",
        );
        assert_eq!(to_char_idx(&r, pos(1, 2)), 5);
        assert_eq!(from_char_idx(&r, 5), pos(1, 2));
        assert_eq!(from_char_idx(&r, 2), pos(0, 2));
        assert_eq!(from_char_idx(&r, 99), pos(1, 3));
    }

    #[test]
    fn long_line_motion_is_local() {
        let mut text = "a".repeat(2_500_000);
        text.push('😀');
        text.push_str(&"a".repeat(2_500_000));
        let r = Rope::from_str(&text);
        let start = std::time::Instant::now();
        let mut p = pos(0, 2_500_000);
        for _ in 0..1000 {
            p = right(&r, p);
        }
        for _ in 0..1000 {
            p = left(&r, p);
        }
        assert_eq!(p, pos(0, 2_500_000));
        assert!(start.elapsed().as_millis() < 200, "{:?}", start.elapsed());
    }

    #[test]
    fn lone_cr_before_newline_is_own_cluster() {
        let r = Rope::from_str("a\r\nb");
        assert_eq!(right(&r, pos(0, 1)), pos(0, 2));
        assert_eq!(right(&r, pos(0, 2)), pos(1, 0));
        assert_eq!(left(&r, pos(0, 2)), pos(0, 1));
    }

    #[test]
    fn empty_rope_motions_noop() {
        let r = Rope::new();
        let z = pos(0, 0);
        assert_eq!(left(&r, z), z);
        assert_eq!(right(&r, z), z);
        assert_eq!(word_left(&r, z), z);
        assert_eq!(word_right(&r, z), z);
        assert_eq!(clamp(&r, pos(3, 3)), z);
        assert_eq!(file_end(&r), z);
        assert_eq!(from_char_idx(&r, 5), z);
        assert_eq!(to_char_idx(&r, z), 0);
    }
}
