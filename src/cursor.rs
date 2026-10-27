//! Pure cursor motion over a rope, in grapheme clusters.
//!
//! `col` is a char index within the line, excluding the trailing `\n`.

use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;

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

fn classify(g: &str) -> Class {
    let c = g.chars().next().unwrap_or(' ');
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// Grapheme clusters of a line as (start col, class), plus the line length in chars.
fn clusters(rope: &Rope, line: usize) -> (Vec<(usize, Class)>, usize) {
    let text = rope.line(line).to_string();
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let mut out = Vec::new();
    let mut col = 0;
    for g in text.graphemes(true) {
        out.push((col, classify(g)));
        col += g.chars().count();
    }
    (out, col)
}

fn last_line(rope: &Rope) -> usize {
    rope.len_lines() - 1
}

fn line_len(rope: &Rope, line: usize) -> usize {
    clusters(rope, line).1
}

pub fn clamp(rope: &Rope, p: Pos) -> Pos {
    let line = p.line.min(last_line(rope));
    let (cl, len) = clusters(rope, line);
    let col = if p.col >= len {
        len
    } else {
        cl.iter()
            .map(|c| c.0)
            .take_while(|&s| s <= p.col)
            .last()
            .unwrap_or(0)
    };
    Pos { line, col }
}

pub fn left(rope: &Rope, p: Pos) -> Pos {
    let p = clamp(rope, p);
    if p.col > 0 {
        let (cl, _) = clusters(rope, p.line);
        let col = cl.iter().map(|c| c.0).take_while(|&s| s < p.col).last();
        Pos {
            line: p.line,
            col: col.unwrap_or(0),
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
    let (cl, len) = clusters(rope, p.line);
    if p.col < len {
        let col = cl.iter().map(|c| c.0).find(|&s| s > p.col).unwrap_or(len);
        Pos { line: p.line, col }
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
    let (cl, _) = clusters(rope, p.line);
    let mut i = cl.iter().take_while(|c| c.0 < p.col).count();
    while i > 0 && cl[i - 1].1 == Class::Space {
        i -= 1;
    }
    if i > 0 {
        let class = cl[i - 1].1;
        while i > 0 && cl[i - 1].1 == class {
            i -= 1;
        }
    }
    Pos {
        line: p.line,
        col: cl.get(i).map_or(0, |c| c.0),
    }
}

pub fn word_right(rope: &Rope, p: Pos) -> Pos {
    let p = clamp(rope, p);
    let (cl, len) = clusters(rope, p.line);
    if p.col >= len {
        return right(rope, p);
    }
    let mut i = cl.iter().take_while(|c| c.0 < p.col).count();
    while i < cl.len() && cl[i].1 == Class::Space {
        i += 1;
    }
    if i < cl.len() {
        let class = cl[i].1;
        while i < cl.len() && cl[i].1 == class {
            i += 1;
        }
    }
    Pos {
        line: p.line,
        col: cl.get(i).map_or(len, |c| c.0),
    }
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
}
