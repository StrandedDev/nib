//! Small hand-written highlighters for Markdown and JSON.

mod json;
mod markdown;

use std::ops::Range;
use std::path::Path;

use ropey::RopeSlice;

use crate::buffer::Buffer;

/// What a span of text is; render.rs maps each to an ANSI colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Normal,
    Heading,
    Emph,
    Code,
    Link,
    ListMark,
    Key,
    String,
    Number,
    Literal,
    Punct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Markdown,
    Json,
}

/// The highlighter for a file, by extension (any case): `.md` and
/// `.markdown` are Markdown, `.json` is JSON; anything else has none.
pub fn lang_for(path: &Path) -> Option<Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" => Some(Lang::Markdown),
        "json" => Some(Lang::Json),
        _ => None,
    }
}

/// State carried from one line to the next: whether a Markdown line sits
/// inside a fenced code block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineState {
    pub in_fence: bool,
}

/// Styled byte ranges of `line`, sorted and not overlapping; text outside
/// them is `Style::Normal`. Updates `state` for the next line.
pub fn highlight_line(kind: Lang, line: &str, state: &mut LineState) -> Vec<(Range<usize>, Style)> {
    match kind {
        Lang::Markdown => markdown::line(line, state),
        Lang::Json => json::line(line),
    }
}

/// Only this many chars at the start of a line are highlighted, so a huge
/// line costs a bounded amount per frame.
pub const LINE_LIMIT: usize = 10_000;

/// Styled char-column ranges of a buffer line (no newline), for drawing.
/// Only the first `LINE_LIMIT` chars are looked at.
pub fn line_styles(
    kind: Lang,
    line: RopeSlice,
    state: &mut LineState,
) -> Vec<(Range<usize>, Style)> {
    let text = line.slice(..line.len_chars().min(LINE_LIMIT)).to_string();
    let mut out = Vec::new();
    let (mut at, mut col) = (0, 0);
    for (r, style) in highlight_line(kind, &text, state) {
        col += text[at..r.start].chars().count();
        let start = col;
        col += text[r.start..r.end].chars().count();
        at = r.end;
        out.push((start..col, style));
    }
    out
}

/// Markdown fence state for lines above the viewport, so a frame never
/// rescans the file: the fence lines seen so far, scanned once and kept
/// until an edit at or above them.
#[derive(Debug, Default)]
pub struct Cache {
    lang: Option<Lang>,
    /// Fence lines below `scanned`, ascending.
    fences: Vec<usize>,
    /// Lines `0..scanned` are scanned.
    scanned: usize,
}

impl Cache {
    /// Brings the cache up to date with `buf` and makes the state at line
    /// `top` known. Takes the buffer's changed-from mark.
    pub fn sync(&mut self, buf: &mut Buffer, lang: Option<Lang>, top: usize) {
        if lang != self.lang {
            *self = Cache {
                lang,
                ..Cache::default()
            };
        }
        let from = buf.take_changed_from();
        self.scanned = self.scanned.min(from);
        self.fences
            .truncate(self.fences.partition_point(|&l| l < from));
        if lang != Some(Lang::Markdown) {
            return;
        }
        let rope = &buf.rope;
        let top = top.min(rope.len_lines());
        for l in self.scanned..top {
            // A fence needs only the first few chars of a line.
            let head: String = rope.line(l).chars().take(6).collect();
            if markdown::is_fence(&head) {
                self.fences.push(l);
            }
        }
        self.scanned = self.scanned.max(top);
    }

    pub fn lang(&self) -> Option<Lang> {
        self.lang
    }

    /// State at the start of `line`, which must be at most the `top` given
    /// to the last `sync`.
    pub fn state_at(&self, line: usize) -> LineState {
        debug_assert!(line <= self.scanned || self.lang != Some(Lang::Markdown));
        let n = self.fences.partition_point(|&l| l < line);
        LineState {
            in_fence: n % 2 == 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(line: &str) -> Vec<(Range<usize>, Style)> {
        highlight_line(Lang::Markdown, line, &mut LineState::default())
    }

    #[test]
    fn fence_state_cached_and_invalidated_from_edit() {
        use crate::cursor::Pos;
        use crate::edit::Editor;
        let mut e = Editor::new(Buffer::from_str_detect("```\na\n```\nb"));
        let mut c = Cache::default();
        c.sync(&mut e.buf, Some(Lang::Markdown), 3);
        assert_eq!(c.state_at(1), LineState { in_fence: true });
        assert_eq!(c.state_at(3), LineState { in_fence: false });
        // Break the closing fence: line 3 is now inside the block.
        e.move_to(Pos { line: 2, col: 0 }, false);
        e.insert_str("x");
        c.sync(&mut e.buf, Some(Lang::Markdown), 3);
        assert_eq!(c.state_at(3), LineState { in_fence: true });
        // Undo restores it.
        e.undo();
        c.sync(&mut e.buf, Some(Lang::Markdown), 3);
        assert_eq!(c.state_at(3), LineState { in_fence: false });
    }

    #[test]
    fn md_heading() {
        assert_eq!(md("# Title"), vec![(0..7, Style::Heading)]);
        assert_eq!(md("### a b"), vec![(0..7, Style::Heading)]);
        // No space after the hashes: not a heading.
        assert_eq!(md("#tag"), vec![]);
    }

    #[test]
    fn md_fence_spans_lines() {
        let mut st = LineState::default();
        let mut hl = |l: &str| highlight_line(Lang::Markdown, l, &mut st);
        assert_eq!(hl("```rust"), vec![(0..7, Style::Code)]);
        // Inside the fence nothing else is recognised.
        assert_eq!(hl("# not a heading"), vec![(0..15, Style::Code)]);
        assert_eq!(hl(""), vec![]);
        assert_eq!(hl("```"), vec![(0..3, Style::Code)]);
        assert_eq!(hl("# Heading"), vec![(0..9, Style::Heading)]);
    }

    #[test]
    fn md_inline_code() {
        assert_eq!(md("a `b` c"), vec![(2..5, Style::Code)]);
        // A run of two backticks closes only on two.
        assert_eq!(md("``x`y``"), vec![(0..7, Style::Code)]);
        // Unclosed: plain text.
        assert_eq!(md("a `b"), vec![]);
        // Emphasis markers inside code are code.
        assert_eq!(md("`*a*`"), vec![(0..5, Style::Code)]);
    }

    #[test]
    fn md_emphasis() {
        assert_eq!(md("a **b c** d"), vec![(2..9, Style::Emph)]);
        assert_eq!(
            md("*a* _b_"),
            vec![(0..3, Style::Emph), (4..7, Style::Emph)]
        );
        // Inside words `_` is just a character; a spaced `*` is not emphasis.
        assert_eq!(md("snake_case_name"), vec![]);
        assert_eq!(md("2 * 3 * 4"), vec![]);
    }

    #[test]
    fn md_link() {
        assert_eq!(md("see [nib](https://x.y) now"), vec![(4..22, Style::Link)]);
        // Brackets without a target are text.
        assert_eq!(md("[a] b"), vec![]);
        assert_eq!(md("[a] [b](c)"), vec![(4..10, Style::Link)]);
    }

    #[test]
    fn md_list_marker() {
        assert_eq!(md("- item"), vec![(0..1, Style::ListMark)]);
        assert_eq!(
            md("  * *em*"),
            vec![(2..3, Style::ListMark), (4..8, Style::Emph)]
        );
        assert_eq!(md("12. x"), vec![(0..3, Style::ListMark)]);
        assert_eq!(md("-x"), vec![]);
    }

    fn json(line: &str) -> Vec<(Range<usize>, Style)> {
        highlight_line(Lang::Json, line, &mut LineState::default())
    }

    #[test]
    fn json_key_vs_string() {
        assert_eq!(
            json(r#"{"a": "b"}"#),
            vec![
                (0..1, Style::Punct),
                (1..4, Style::Key),
                (4..5, Style::Punct),
                (6..9, Style::String),
                (9..10, Style::Punct),
            ]
        );
        // An escaped quote does not end the string; space before `:` is fine.
        assert_eq!(
            json(r#""a\"b" :"#),
            vec![(0..6, Style::Key), (7..8, Style::Punct)]
        );
        // Unterminated: the string runs to the line end.
        assert_eq!(
            json(r#"["ab"#),
            vec![(0..1, Style::Punct), (1..4, Style::String)]
        );
    }

    #[test]
    fn json_number_and_literal() {
        assert_eq!(
            json("[-1.5e+3, true, null]"),
            vec![
                (0..1, Style::Punct),
                (1..8, Style::Number),
                (8..9, Style::Punct),
                (10..14, Style::Literal),
                (14..15, Style::Punct),
                (16..20, Style::Literal),
                (20..21, Style::Punct),
            ]
        );
        assert_eq!(json("false"), vec![(0..5, Style::Literal)]);
        // A word that is not a literal stays plain, even when it starts with one.
        assert_eq!(json("nullx truth"), vec![]);
    }

    #[test]
    fn unknown_ext_no_highlight() {
        let lang = |p: &str| lang_for(Path::new(p));
        assert_eq!(lang("notes.txt"), None);
        assert_eq!(lang("Makefile"), None);
        assert_eq!(lang("md"), None);
        assert_eq!(lang("a/README.md"), Some(Lang::Markdown));
        assert_eq!(lang("x.Markdown"), Some(Lang::Markdown));
        assert_eq!(lang("package.JSON"), Some(Lang::Json));
    }
}
