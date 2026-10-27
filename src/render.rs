//! Draws one frame from app state. Text layout comes from `layout`; this
//! module only turns cells into terminal output.

use std::fmt;
use std::io::{self, Write};

use crossterm::{
    Command,
    cursor::{Hide, MoveTo, Show},
    queue,
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
    terminal::{Clear, ClearType},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::cli;
use crate::cursor::line_text;
use crate::edit::Editor;
use crate::layout::{self, Row, Viewport};
use crate::prompt::Prompt;

/// Shown in place of a control char so it never reaches the terminal raw.
const CONTROL_MARK: &str = "\u{b7}";

/// Bright black foreground (SGR 90), one of the 16 ANSI colours. crossterm
/// writes `Color::DarkGrey` as 256-colour index 8, which nib does not use.
struct MarkColour;

impl Command for MarkColour {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("[90m")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// What one frame needs, borrowed from `App`.
pub struct AppView<'a> {
    pub editor: &'a Editor,
    pub viewport: &'a Viewport,
    /// Terminal size; the text area is `height - 2` rows.
    pub screen: (u16, u16),
    pub name: String,
    pub state: String,
    pub status: Option<&'a str>,
    pub prompt: Option<&'a Prompt>,
}

pub fn draw(out: &mut impl Write, app: &AppView) -> io::Result<()> {
    let (w, h) = app.screen;
    queue!(out, Hide)?;
    if w == 0 || h == 0 {
        return out.flush();
    }
    let title = title_bar(usize::from(w), &app.name, &app.state);
    queue!(
        out,
        MoveTo(0, 0),
        SetAttribute(Attribute::Reverse),
        Print(title),
        SetAttribute(Attribute::NoReverse)
    )?;

    let text_h = h.saturating_sub(2);
    let rope = &app.editor.buf.rope;
    let rows = layout::visible_rows(rope, app.viewport);
    for y in 0..text_h {
        queue!(out, MoveTo(0, y + 1))?;
        let used = match rows.get(usize::from(y)) {
            Some(row) => draw_row(out, app, row)?,
            None => 0,
        };
        if used < usize::from(w) {
            queue!(out, Clear(ClearType::UntilNewLine))?;
        }
    }

    let mut cursor = None;
    if h >= 2 {
        cursor = draw_status(out, app, w, h - 1)?;
    }
    if cursor.is_none() && text_h > 0 {
        let (x, y) = layout::cursor_screen(rope, app.viewport, app.editor.cursor);
        if y < text_h {
            cursor = Some((x.min(w - 1), y + 1));
        }
    }
    if let Some((x, y)) = cursor {
        queue!(out, MoveTo(x, y), Show)?;
    }
    out.flush()
}

/// Draws one text row from the cursor's current position. Returns the
/// number of cells written.
fn draw_row(out: &mut impl Write, app: &AppView, row: &Row) -> io::Result<usize> {
    let rope = &app.editor.buf.rope;
    let s = line_text(rope, row.line);
    let mut x = 0;
    for c in layout::draw_cells(rope, app.viewport, row) {
        if c.x > x {
            queue!(out, Print(" ".repeat(c.x - x)))?;
        }
        x = c.x + c.w;
        let g = s.slice(c.start..c.end);
        if g.char(0) == '\t' {
            queue!(out, Print(" ".repeat(c.w)))?;
        } else if g.chars().any(char::is_control) {
            queue!(
                out,
                MarkColour,
                Print(CONTROL_MARK),
                SetForegroundColor(Color::Reset)
            )?;
            if c.w > 1 {
                queue!(out, Print(" ".repeat(c.w - 1)))?;
            }
        } else if c.w > 0 {
            queue!(out, Print(g))?;
        } else {
            // A lone zero-width grapheme would merge with its neighbour.
        }
    }
    Ok(x)
}

/// Draws the status line at row `y`. Returns the cursor cell when a prompt
/// is open. The last column is left empty so no terminal scrolls.
fn draw_status(
    out: &mut impl Write,
    app: &AppView,
    w: u16,
    y: u16,
) -> io::Result<Option<(u16, u16)>> {
    let max = usize::from(w) - 1;
    let (line, cursor_x) = match app.prompt {
        Some(Prompt::Text {
            label,
            input,
            cursor,
        }) => {
            let (line, x) = prompt_line(label, input, *cursor, max);
            (line, Some(x))
        }
        Some(Prompt::Choice { label, .. }) => {
            let line = fit_right(&clean(label), max);
            let x = line.width().min(max);
            (line, Some(x))
        }
        None => (fit_right(&clean(app.status.unwrap_or("")), max), None),
    };
    queue!(
        out,
        MoveTo(0, y),
        Print(line),
        Clear(ClearType::UntilNewLine)
    )?;
    let clamp = |x: usize| u16::try_from(x).unwrap_or(u16::MAX);
    Ok(cursor_x.map(|x| (clamp(x), y)))
}

/// Label then input, scrolled so the cursor stays on screen. Returns the
/// line and the cursor's x.
fn prompt_line(label: &str, input: &str, cursor: usize, max: usize) -> (String, usize) {
    let label = fit_right(&clean(label), max);
    let lw = label.width();
    let before = clean(&input[..cursor]);
    let after = clean(&input[cursor..]);
    let (before, x) = if lw + before.width() < max {
        let x = lw + before.width();
        (before, x)
    } else {
        let room = max.saturating_sub(lw + 2);
        let kept = fit_left(&before, room, "");
        let x = lw + 1 + kept.width();
        (format!("\u{2026}{kept}"), x)
    };
    let line = fit_right(&format!("{label}{before}{after}"), max);
    (line, x.min(max))
}

/// The title row, exactly `w` cells when it fits: version left, name centred
/// (cut from the left with an ellipsis), state right.
fn title_bar(w: usize, name: &str, state: &str) -> String {
    let mut left = format!(" {}", cli::version_text());
    let right = fit_right(&format!("{} ", clean(state)), w);
    if left.width() + right.width() + 1 > w {
        left.clear();
    }
    let (lw, rw) = (left.width(), right.width());
    let gap = w - lw - rw;
    let name = fit_left(&clean(name), gap.saturating_sub(2), "\u{2026}");
    let nw = name.width();
    let lo = if lw > 0 { lw + 1 } else { 0 };
    let hi = (w - rw).saturating_sub(nw + 1).max(lo);
    let x = (w.saturating_sub(nw) / 2).clamp(lo, hi).min(w - rw - nw);
    format!(
        "{left}{}{name}{}{right}",
        " ".repeat(x - lw),
        " ".repeat(w - rw - x - nw)
    )
}

/// Control chars replaced by a visible mark.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{b7}' } else { c })
        .collect()
}

/// The longest grapheme prefix of `s` at most `max` cells wide.
fn fit_right(s: &str, max: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for g in s.graphemes(true) {
        used += g.width();
        if used > max {
            break;
        }
        out.push_str(g);
    }
    out
}

/// `s` cut from the left to at most `max` cells, with `mark` in front when
/// anything was cut.
fn fit_left(s: &str, max: usize, mark: &str) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let room = max.saturating_sub(mark.width());
    if room == 0 {
        return String::new();
    }
    let mut used = 0;
    let mut start = s.len();
    for (i, g) in s.grapheme_indices(true).rev() {
        used += g.width();
        if used > room {
            break;
        }
        start = i;
    }
    format!("{mark}{}", &s[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_bar_layout() {
        let t = title_bar(40, "todo.md", "Modified");
        assert_eq!(t.width(), 40);
        assert!(t.starts_with(" nib "), "{t:?}");
        assert!(t.ends_with("Modified "), "{t:?}");
        let at = t.find("todo.md").unwrap();
        assert!((15..=18).contains(&at), "{t:?}");
    }

    #[test]
    fn title_bar_cuts_name_from_left_and_survives_tiny_widths() {
        let t = title_bar(30, "a-very-long-file-name.md", "New File");
        assert_eq!(t.width(), 30);
        assert!(t.contains("\u{2026}"), "{t:?}");
        assert!(t.contains("name.md"), "{t:?}");
        for w in 0..12 {
            assert!(title_bar(w, "x\u{1b}y", "Modified [Read-only]").width() <= w);
        }
        assert!(!title_bar(40, "x\u{1b}y", "").contains('\u{1b}'));
    }
}
