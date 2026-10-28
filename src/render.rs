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
use crate::cursor::{Pos, from_char_idx, line_text};
use crate::edit::Editor;
use crate::find::FindSession;
use crate::highlight::{self, Cache, Style};
use crate::layout::{self, Row, Viewport};
use crate::palette::Item;
use crate::prompt::Prompt;

/// Shown in place of a control char so it never reaches the terminal raw.
const CONTROL_MARK: &str = "\u{b7}";

/// Bright black foreground (SGR 90), one of the 16 ANSI colours. crossterm
/// writes `Color::DarkGrey` as 256-colour index 8, which nib does not use.
pub(crate) struct MarkColour;

impl Command for MarkColour {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("[90m")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Sets the foreground with a raw SGR code, so only the 16 ANSI colours
/// are ever written (crossterm writes named colours as 256-colour indices).
struct Fg(&'static str);

impl Command for Fg {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{}m", self.0)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// The terminal's default foreground.
const FG_DEFAULT: &str = "39";
/// Bright black, for control marks.
const FG_MARK: &str = "90";

/// SGR foreground code for a highlight style.
fn style_fg(style: Style) -> &'static str {
    match style {
        Style::Normal => FG_DEFAULT,
        Style::Heading | Style::Literal => "35",
        Style::Emph | Style::Number => "33",
        Style::Code | Style::String => "32",
        Style::Link | Style::Key => "34",
        Style::ListMark => "36",
        Style::Punct => FG_MARK,
    }
}

/// Gutter width for `lines` lines: the digits of the last line number and
/// one space.
pub fn gutter_width(lines: usize) -> u16 {
    let digits = lines.max(1).ilog10() + 1;
    u16::try_from(digits).unwrap_or(u16::MAX) + 1
}

/// What one frame needs, borrowed from `App`.
pub struct AppView<'a> {
    pub editor: &'a Editor,
    pub viewport: &'a Viewport,
    /// Terminal size; the text area is `height - 2` rows.
    pub screen: (u16, u16),
    /// Columns of line numbers left of the text; 0 when off.
    pub gutter: u16,
    pub name: String,
    pub state: String,
    pub status: Option<&'a str>,
    pub prompt: Option<&'a Prompt>,
    /// The open find/replace session: its matches are underlined.
    pub find: Option<&'a FindSession>,
    pub palette: Option<PaletteView<'a>>,
    /// Syntax highlighting, with fence state known up to the top line.
    pub syntax: Option<&'a Cache>,
}

/// The open command palette, ready to draw.
pub struct PaletteView<'a> {
    pub query: &'a str,
    pub selected: usize,
    pub items: Vec<Item>,
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
    let mut syntax = app
        .syntax
        .and_then(|c| Some((c.lang()?, c.state_at(rows.first()?.line))));
    // Styles of the line being drawn, worked out once for all its rows.
    let mut styled = (usize::MAX, Vec::new());
    for y in 0..text_h {
        queue!(out, MoveTo(0, y + 1))?;
        let used = match rows.get(usize::from(y)) {
            Some(row) => {
                if let Some((lang, state)) = &mut syntax {
                    if styled.0 != row.line {
                        let s = line_text(rope, row.line);
                        styled = (row.line, highlight::line_styles(*lang, s, state));
                    }
                }
                draw_gutter(out, row, app.gutter)? + draw_row(out, app, row, &styled.1)?
            }
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
    if let Some(pal) = &app.palette {
        cursor = draw_palette(out, pal, (w, h))?.or(cursor);
    }
    if cursor.is_none() && text_h > 0 {
        if let Some((x, y)) = layout::cursor_screen(rope, app.viewport, app.editor.cursor) {
            if y < text_h {
                cursor = Some((x.saturating_add(app.gutter).min(w - 1), y + 1));
            }
        }
    }
    if let Some((x, y)) = cursor {
        queue!(out, MoveTo(x, y), Show)?;
    }
    out.flush()
}

const PALETTE_WIDTH: u16 = 60;
const PALETTE_ROWS: usize = 10;

/// Draws the palette box from the top centre, below the title bar: a query
/// line, then up to ten commands, the selected one in reverse video. A
/// terminal under 20x5 gets no border. Returns the cursor cell.
fn draw_palette(
    out: &mut impl Write,
    pal: &PaletteView,
    (w, h): (u16, u16),
) -> io::Result<Option<(u16, u16)>> {
    let avail = usize::from(h.saturating_sub(2));
    if w == 0 || avail == 0 {
        return Ok(None);
    }
    let border = w >= 20 && h >= 5;
    let width = if border { PALETTE_WIDTH.min(w - 2) } else { w };
    let x0 = (w - width) / 2;
    let inner = usize::from(width) - if border { 2 } else { 0 };
    let rows = PALETTE_ROWS.min(avail - if border { 3.min(avail) } else { 1 });
    let bx = BoxCols { x0, border, inner };
    let mut y = 1;
    if border {
        bx.edge(out, y, ('\u{250c}', '\u{2510}'))?;
        y += 1;
    }
    let query = fit_left(
        &format!("> {}", clean(pal.query)),
        inner.saturating_sub(1),
        "\u{2026}",
    );
    let cursor = (
        x0 + u16::from(border) + u16::try_from(query.width()).unwrap_or(0),
        y,
    );
    bx.side(out, y, &pad(&query, inner), false)?;
    // Scroll so the selected row stays in view.
    let first = (pal.selected + 1).saturating_sub(rows);
    for (i, item) in pal.items.iter().enumerate().skip(first).take(rows) {
        y += 1;
        bx.side(out, y, &item_line(item, inner), i == pal.selected)?;
    }
    if border {
        bx.edge(out, y + 1, ('\u{2514}', '\u{2518}'))?;
    }
    Ok(Some(cursor))
}

/// Where the palette box sits: its left column, border and inner width.
struct BoxCols {
    x0: u16,
    border: bool,
    inner: usize,
}

impl BoxCols {
    /// One content row between the vertical borders, if any.
    fn side(&self, out: &mut impl Write, y: u16, text: &str, rev: bool) -> io::Result<()> {
        let bar = if self.border { "\u{2502}" } else { "" };
        queue!(out, MoveTo(self.x0, y), Print(bar))?;
        if rev {
            queue!(out, SetAttribute(Attribute::Reverse))?;
        }
        queue!(out, Print(text))?;
        if rev {
            queue!(out, SetAttribute(Attribute::NoReverse))?;
        }
        queue!(out, Print(bar))
    }

    /// The top or bottom border.
    fn edge(&self, out: &mut impl Write, y: u16, (l, r): (char, char)) -> io::Result<()> {
        let line = "\u{2500}".repeat(self.inner);
        queue!(out, MoveTo(self.x0, y), Print(format!("{l}{line}{r}")))
    }
}

/// `s` padded with spaces to exactly `width` cells.
fn pad(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.width())))
}

/// Name on the left, keys on the right, `width` cells in all.
fn item_line(item: &Item, width: usize) -> String {
    let name = fit_right(&clean(item.name), width);
    let keys = clean(&item.keys);
    let keys = if name.width() + 1 + keys.width() <= width {
        keys
    } else {
        String::new()
    };
    let gap = width - name.width() - keys.width();
    format!("{name}{}{keys}", " ".repeat(gap))
}

/// Draws the line number of `row` right-aligned in `width` columns, or a
/// blank gutter on a wrapped continuation row. Returns the cells written.
fn draw_gutter(out: &mut impl Write, row: &Row, width: u16) -> io::Result<usize> {
    let width = usize::from(width);
    if width == 0 {
        return Ok(0);
    }
    if row.start_col == 0 {
        let n = format!("{:>w$} ", row.line + 1, w = width - 1);
        queue!(
            out,
            MarkColour,
            Print(fit_right(&n, width)),
            SetForegroundColor(Color::Reset)
        )?;
    } else {
        queue!(out, Print(" ".repeat(width)))?;
    }
    Ok(width)
}

/// Switches an attribute on or off, writing only on a change.
fn set_attr(
    out: &mut impl Write,
    state: &mut bool,
    want: bool,
    (on, off): (Attribute, Attribute),
) -> io::Result<()> {
    if *state != want {
        queue!(out, SetAttribute(if want { on } else { off }))?;
        *state = want;
    }
    Ok(())
}

const REVERSE: (Attribute, Attribute) = (Attribute::Reverse, Attribute::NoReverse);
const UNDERLINE: (Attribute, Attribute) = (Attribute::Underlined, Attribute::NoUnderline);

/// Switches the foreground, writing only on a change.
fn set_fg(out: &mut impl Write, state: &mut &'static str, want: &'static str) -> io::Result<()> {
    if *state != want {
        queue!(out, Fg(want))?;
        *state = want;
    }
    Ok(())
}

/// Draws one text row from the cursor's current position. Returns the
/// number of cells written. Selected cells, and the line end when its
/// newline is selected, are drawn in reverse video without syntax colour.
/// `styles` are the line's highlight ranges in char columns, sorted.
fn draw_row(
    out: &mut impl Write,
    app: &AppView,
    row: &Row,
    styles: &[(std::ops::Range<usize>, Style)],
) -> io::Result<usize> {
    let rope = &app.editor.buf.rope;
    let s = line_text(rope, row.line);
    let sel = app
        .editor
        .selection()
        .map(|r| (from_char_idx(rope, r.start), from_char_idx(rope, r.end)));
    let selected = |col| {
        let p = Pos {
            line: row.line,
            col,
        };
        sel.is_some_and(|(a, b)| a <= p && p < b)
    };
    let base = rope.line_to_char(row.line);
    let cells: Vec<_> = layout::draw_cells(rope, app.viewport, row).collect();
    // Matches are looked for only under the cells on screen, so a huge
    // no-wrap line costs one screen width, not its length.
    let marks = match (app.find, cells.first(), cells.last()) {
        (Some(f), Some(a), Some(b)) => f.visible_matches(rope, base + a.start..base + b.end),
        _ => Vec::new(),
    };
    // `marks` are sorted and do not overlap.
    let marked = |col: usize| {
        let i = base + col;
        let k = marks.partition_point(|m| m.end <= i);
        marks.get(k).is_some_and(|m| m.start <= i)
    };
    let style_at = |col: usize| {
        let k = styles.partition_point(|(r, _)| r.end <= col);
        match styles.get(k) {
            Some((r, st)) if r.start <= col => *st,
            _ => Style::Normal,
        }
    };
    let (mut rev, mut under, mut fg) = (false, false, FG_DEFAULT);
    let mut x = 0;
    for c in cells {
        if c.x > x {
            set_attr(out, &mut rev, false, REVERSE)?;
            set_attr(out, &mut under, false, UNDERLINE)?;
            queue!(out, Print(" ".repeat(c.x - x)))?;
        }
        x = c.x + c.w;
        let sel = selected(c.start);
        set_attr(out, &mut rev, sel, REVERSE)?;
        set_attr(out, &mut under, marked(c.start), UNDERLINE)?;
        let colour = if sel {
            FG_DEFAULT
        } else {
            style_fg(style_at(c.start))
        };
        let g = s.slice(c.start..c.end);
        if g.char(0) == '\t' {
            queue!(out, Print(" ".repeat(c.w)))?;
        } else if g.chars().any(char::is_control) {
            set_fg(out, &mut fg, FG_MARK)?;
            queue!(out, Print(CONTROL_MARK))?;
            if c.w > 1 {
                queue!(out, Print(" ".repeat(c.w - 1)))?;
            }
        } else if c.w > 0 {
            set_fg(out, &mut fg, colour)?;
            queue!(out, Print(g))?;
        } else {
            // A lone zero-width grapheme would merge with its neighbour.
        }
    }
    let len = s.len_chars();
    if row.end_col == len && x < usize::from(app.viewport.width) && selected(len) {
        set_attr(out, &mut rev, true, REVERSE)?;
        set_attr(out, &mut under, false, UNDERLINE)?;
        set_fg(out, &mut fg, FG_DEFAULT)?;
        queue!(out, Print(' '))?;
        x += 1;
    }
    set_attr(out, &mut rev, false, REVERSE)?;
    set_attr(out, &mut under, false, UNDERLINE)?;
    set_fg(out, &mut fg, FG_DEFAULT)?;
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
pub(crate) fn clean(s: &str) -> String {
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
pub(crate) fn fit_left(s: &str, max: usize, mark: &str) -> String {
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

    fn frame(text: &str, width: u16, sel: (Pos, Pos)) -> String {
        let mut e = Editor::new(crate::buffer::Buffer::from_str_detect(text));
        e.move_to(sel.0, false);
        e.move_to(sel.1, true);
        let vp = Viewport::new(width, 6, true, 4);
        let view = AppView {
            editor: &e,
            viewport: &vp,
            screen: (width, 8),
            gutter: 0,
            name: String::new(),
            state: String::new(),
            status: None,
            prompt: None,
            find: None,
            palette: None,
            syntax: None,
        };
        let mut out = Vec::new();
        draw(&mut out, &view).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn selection_drawn_in_reverse_video() {
        let f = frame("hello", 20, (pos(0, 1), pos(0, 3)));
        assert!(f.contains("h\u{1b}[7mel\u{1b}[27mlo"), "{f:?}");
        let none = frame("hello", 20, (pos(0, 1), pos(0, 1)));
        assert!(!none.contains("h\u{1b}[7m"), "{none:?}");
    }

    #[test]
    fn selection_covers_line_ends_and_wrapped_rows() {
        // The newline of "ab" is selected: one reversed space after it.
        let f = frame("ab\ncd", 20, (pos(0, 1), pos(1, 1)));
        assert!(f.contains("a\u{1b}[7mb \u{1b}[27m"), "{f:?}");
        assert!(f.contains("\u{1b}[7mc\u{1b}[27md"), "{f:?}");
        // An empty line inside the selection shows as one reversed space.
        let f = frame("a\n\nb", 20, (pos(0, 0), pos(2, 0)));
        assert!(f.contains("\u{1b}[7m \u{1b}[27m"), "{f:?}");
        // Wrapped: both rows carry reverse video.
        let f = frame("abcdefgh", 4, (pos(0, 2), pos(0, 6)));
        assert!(f.contains("ab\u{1b}[7mcd\u{1b}[27m"), "{f:?}");
        assert!(f.contains("\u{1b}[7mef\u{1b}[27mgh"), "{f:?}");
    }

    #[test]
    fn find_matches_underlined_current_match_reversed() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut e = Editor::new(crate::buffer::Buffer::from_str_detect("ab cd ab"));
        let mut f = FindSession::new(&e, false);
        for c in ['a', 'b'] {
            let k = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            f.handle_key(&k, false, &mut e);
        }
        let vp = Viewport::new(20, 6, true, 4);
        let view = AppView {
            editor: &e,
            viewport: &vp,
            screen: (20, 8),
            gutter: 0,
            name: String::new(),
            state: String::new(),
            status: None,
            prompt: Some(f.prompt()),
            find: Some(&f),
            palette: None,
            syntax: None,
        };
        let mut out = Vec::new();
        draw(&mut out, &view).unwrap();
        let out = String::from_utf8(out).unwrap();
        // The current match is selected and underlined; the other only underlined.
        assert!(out.contains("\u{1b}[7m\u{1b}[4mab\u{1b}[27m"), "{out:?}");
        assert!(out.contains("\u{1b}[4mab\u{1b}[24m"), "{out:?}");
    }

    #[test]
    fn line_numbers_drawn_in_gutter() {
        let e = Editor::new(crate::buffer::Buffer::from_str_detect("abcdef\nx"));
        // Text area of 4 columns after a 2-column gutter.
        let vp = Viewport::new(4, 6, true, 4);
        let view = AppView {
            editor: &e,
            viewport: &vp,
            screen: (6, 8),
            gutter: 2,
            name: String::new(),
            state: String::new(),
            status: None,
            prompt: None,
            find: None,
            palette: None,
            syntax: None,
        };
        let mut out = Vec::new();
        draw(&mut out, &view).unwrap();
        let out = String::from_utf8(out).unwrap();
        let num = |n: &str| format!("\u{1b}[90m{n}\u{1b}[39m");
        assert!(out.contains(&format!("{}abcd", num("1 "))), "{out:?}");
        // A wrapped continuation row has a blank gutter.
        assert!(out.contains("\u{1b}[3;1H  ef"), "{out:?}");
        assert!(out.contains(&format!("{}x", num("2 "))), "{out:?}");
        // The cursor sits after the gutter.
        assert!(out.ends_with("\u{1b}[2;3H\u{1b}[?25h"), "{out:?}");
    }

    #[test]
    fn nowrap_render_highlight_is_bounded() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // Two huge lines: the cursor's line carries the no-wrap anchor, the
        // other one does not.
        let line = "ab ".repeat(5 * 1024 * 1024 / 6);
        let text = format!("{line}\n{line}");
        let mut e = Editor::new(crate::buffer::Buffer::from_str_detect(&text));
        e.move_to(pos(1, line.len() / 2), false);
        let mut vp = Viewport::new(80, 6, false, 4);
        layout::scroll_to_cursor(&e.buf.rope, &mut vp, e.cursor, e.buf.version);
        let mut f = FindSession::new(&e, false);
        for c in ['a', 'b'] {
            let k = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            f.handle_key(&k, false, &mut e);
        }
        let view = AppView {
            editor: &e,
            viewport: &vp,
            screen: (80, 8),
            gutter: 0,
            name: String::new(),
            state: String::new(),
            status: None,
            prompt: Some(f.prompt()),
            find: Some(&f),
            palette: None,
            syntax: None,
        };
        let mut out = Vec::new();
        let t = std::time::Instant::now();
        draw(&mut out, &view).unwrap();
        let took = t.elapsed();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("\u{1b}[4mab\u{1b}[24m"),
            "matches still underlined"
        );
        assert!(took < std::time::Duration::from_millis(50), "{took:?}");
    }

    /// A frame of `e` as Markdown, the window starting at line `top`.
    fn markdown_frame(e: &mut Editor, top: usize, wrap: bool) -> String {
        use crate::highlight::{Cache, Lang};
        let mut vp = Viewport::new(20, 3, wrap, 4);
        vp.top.line = top;
        let mut syntax = Cache::default();
        syntax.sync(&mut e.buf, Some(Lang::Markdown), top);
        let view = AppView {
            editor: e,
            viewport: &vp,
            screen: (20, 5),
            gutter: 0,
            name: String::new(),
            state: String::new(),
            status: None,
            prompt: None,
            find: None,
            palette: None,
            syntax: Some(&syntax),
        };
        let mut out = Vec::new();
        draw(&mut out, &view).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn markdown_drawn_in_ansi_colours() {
        let text = "```\ncode\n```\n# Hi";
        let mut e = Editor::new(crate::buffer::Buffer::from_str_detect(text));
        // The fence opens above the window: "code" is still code (green).
        let f = markdown_frame(&mut e, 1, true);
        assert!(f.contains("\u{1b}[2;1H\u{1b}[32mcode\u{1b}[39m"), "{f:?}");
        assert!(f.contains("\u{1b}[4;1H\u{1b}[35m# Hi\u{1b}[39m"), "{f:?}");
        // Selected cells drop their colour so the selection reads as one block.
        e.move_to(pos(3, 2), false);
        e.move_to(pos(3, 4), true);
        let f = markdown_frame(&mut e, 1, true);
        assert!(f.contains("\u{1b}[35m# \u{1b}[7m\u{1b}[39mHi"), "{f:?}");
    }

    #[test]
    fn markdown_huge_line_draws_fast() {
        let line = "*a* `b` ".repeat(5 * 1024 * 1024 / 8);
        let mut e = Editor::new(crate::buffer::Buffer::from_str_detect(&line));
        let t = std::time::Instant::now();
        let f = markdown_frame(&mut e, 0, true);
        let took = t.elapsed();
        assert!(f.contains("\u{1b}[33m*a*"), "{:?}", &f[..200]);
        assert!(took < std::time::Duration::from_millis(50), "{took:?}");
    }

    fn palette_frame(query: &str, selected: usize, screen: (u16, u16)) -> String {
        use crate::keymap::Keymap;
        use crate::palette::Palette;
        let e = Editor::new(crate::buffer::Buffer::from_str_detect("text"));
        let vp = Viewport::new(screen.0, screen.1.saturating_sub(2), true, 4);
        let pal = Palette {
            query: query.into(),
            selected,
        };
        let view = AppView {
            editor: &e,
            viewport: &vp,
            screen,
            gutter: 0,
            name: String::new(),
            state: String::new(),
            status: None,
            prompt: None,
            find: None,
            palette: Some(PaletteView {
                query,
                selected,
                items: pal.items(&Keymap::defaults()),
            }),
            syntax: None,
        };
        let mut out = Vec::new();
        draw(&mut out, &view).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn palette_overlay_has_border_query_and_selected_row() {
        let f = palette_frame("sa", 0, (80, 24));
        // 60 wide, centred: starts at column 10, below the title bar.
        assert!(f.contains("\u{1b}[2;11H\u{250c}"), "{f:?}");
        assert!(f.contains("> sa"), "{f:?}");
        // "Save" outranks "Save As"; the selected row is reversed, keys right-aligned.
        assert!(f.contains("\u{2502}\u{1b}[7mSave  "), "{f:?}");
        assert!(f.contains("Ctrl+S\u{1b}[27m\u{2502}"), "{f:?}");
        assert!(f.contains("\u{2502}Save As  "), "{f:?}");
        // The cursor sits after the query.
        assert!(f.ends_with("\u{1b}[3;16H\u{1b}[?25h"), "{f:?}");
    }

    #[test]
    fn palette_narrow_terminal() {
        // Too small for a border: the box is drawn without one.
        let f = palette_frame("q", 0, (15, 6));
        assert!(!f.contains('\u{250c}') && !f.contains('\u{2502}'), "{f:?}");
        assert!(f.contains("> q"), "{f:?}");
        for w in 0..30 {
            for h in 0..12 {
                let f = palette_frame("zz\u{1b}", 3, (w, h));
                assert!(!f.contains("zz\u{1b}"));
            }
        }
    }
}
