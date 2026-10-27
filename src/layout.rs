//! Soft-wrap layout: buffer lines to screen rows, and back.
//!
//! Columns are char indices within a line (excluding the newline). Every walk
//! goes through `Cells`, a grapheme iterator over rope chunks that starts at
//! an arbitrary column, so cost follows the rows asked for, not line length.

use crate::cursor::{Pos, clamp, line_text, next_col};
use ropey::{Rope, RopeSlice};
use unicode_width::UnicodeWidthStr;

/// One screen row: chars `start_col..end_col` of `line` (`end_col` exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Row {
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
}

/// One grapheme: chars `start..end`, at display column `x` (relative to the
/// row start) and `w` cells wide.
struct Cell {
    start: usize,
    end: usize,
    x: usize,
    w: usize,
}

struct Cells<'a> {
    s: RopeSlice<'a>,
    col: usize,
    x: usize,
    tab: usize,
}

impl<'a> Cells<'a> {
    fn new(s: RopeSlice<'a>, col: usize, tab_width: u8) -> Self {
        Cells {
            s,
            col: col.min(s.len_chars()),
            x: 0,
            tab: usize::from(tab_width).max(1),
        }
    }
}

impl Iterator for Cells<'_> {
    type Item = Cell;

    fn next(&mut self) -> Option<Cell> {
        if self.col >= self.s.len_chars() {
            return None;
        }
        let end = next_col(self.s, self.col);
        let g = self.s.slice(self.col..end);
        // A lone combining mark is width 0 but still occupies a col; control
        // chars other than tab (a literal CR) display as width 1.
        let w = if g.len_chars() == 1 && g.char(0) == '\t' {
            self.tab - self.x % self.tab
        } else if g.len_chars() == 1 && g.char(0).is_control() {
            1
        } else {
            g.to_string().width()
        };
        let cell = Cell {
            start: self.col,
            end,
            x: self.x,
            w,
        };
        self.col = end;
        self.x += w;
        Some(cell)
    }
}

/// The row of `line` starting at `start`.
fn row_from(s: RopeSlice, line: usize, start: usize, width: u16, tab: u8, wrap: bool) -> Row {
    let len = s.len_chars();
    if !wrap {
        return Row {
            line,
            start_col: 0,
            end_col: len,
        };
    }
    let start = start.min(len);
    let width = usize::from(width).max(1);
    let mut end = start;
    for c in Cells::new(s, start, tab) {
        // A grapheme wider than the row still gets a row of its own.
        if c.x > 0 && (c.x >= width || c.x + c.w > width) {
            break;
        }
        end = c.end;
    }
    Row {
        line,
        start_col: start,
        end_col: end,
    }
}

pub fn rows_for_line(
    line: &str,
    line_idx: usize,
    width: u16,
    wrap: bool,
    tab_width: u8,
) -> Vec<Row> {
    let rope = Rope::from_str(line);
    let s = rope.slice(..);
    let len = s.len_chars();
    let mut rows = Vec::new();
    let mut start = 0;
    loop {
        let r = row_from(s, line_idx, start, width, tab_width, wrap);
        rows.push(r);
        if r.end_col >= len {
            return rows;
        }
        start = r.end_col;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub top: Row,
    pub width: u16,
    pub height: u16,
    pub wrap: bool,
    /// Display columns scrolled off the left edge (no-wrap mode only).
    pub hscroll: usize,
    pub tab_width: u8,
}

/// Rows from `(line, start)` onward through the rest of the buffer, computed
/// lazily one row at a time.
struct RowIter<'a> {
    rope: &'a Rope,
    line: usize,
    start: usize,
    width: u16,
    tab: u8,
    wrap: bool,
}

impl<'a> RowIter<'a> {
    fn new(rope: &'a Rope, vp: &Viewport, line: usize, start: usize) -> Self {
        RowIter {
            rope,
            line,
            start: if vp.wrap { start } else { 0 },
            width: vp.width,
            tab: vp.tab_width,
            wrap: vp.wrap,
        }
    }
}

impl Iterator for RowIter<'_> {
    type Item = Row;

    fn next(&mut self) -> Option<Row> {
        if self.line >= self.rope.len_lines() {
            return None;
        }
        let s = line_text(self.rope, self.line);
        let row = row_from(s, self.line, self.start, self.width, self.tab, self.wrap);
        if row.end_col < s.len_chars() {
            self.start = row.end_col;
        } else {
            self.line += 1;
            self.start = 0;
        }
        Some(row)
    }
}

/// At most `height` rows, starting at `vp.top`.
pub fn visible_rows(rope: &Rope, vp: &Viewport) -> Vec<Row> {
    RowIter::new(rope, vp, vp.top.line, vp.top.start_col)
        .take(usize::from(vp.height))
        .collect()
}

fn clamp_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// Whether `cur` (already clamped) is shown on `row`. The end-of-line
/// position belongs to the last row of its line.
fn row_holds(rope: &Rope, row: &Row, cur: Pos) -> bool {
    row.line == cur.line
        && cur.col >= row.start_col
        && (cur.col < row.end_col
            || (cur.col == row.end_col && row.end_col == line_text(rope, row.line).len_chars()))
}

/// Display x of char column `col`, counted from the start of the row that
/// begins at `row_start`.
fn x_at(s: RopeSlice, row_start: usize, col: usize, tab: u8) -> usize {
    Cells::new(s, row_start, tab)
        .take_while(|c| c.start < col)
        .last()
        .map_or(0, |c| c.x + c.w)
}

/// Screen cell (x, y) of the cursor relative to the text area. Off-screen
/// cursors report (0, 0); call `scroll_to_cursor` first.
pub fn cursor_screen(rope: &Rope, vp: &Viewport, cur: Pos) -> (u16, u16) {
    let cur = clamp(rope, cur);
    let s = line_text(rope, cur.line);
    if !vp.wrap {
        let y = cur.line.saturating_sub(vp.top.line);
        let x = x_at(s, 0, cur.col, vp.tab_width).saturating_sub(vp.hscroll);
        return (clamp_u16(x), clamp_u16(y));
    }
    let rows = RowIter::new(rope, vp, vp.top.line, vp.top.start_col).take(usize::from(vp.height));
    for (y, row) in rows.enumerate() {
        if row_holds(rope, &row, cur) {
            let x = x_at(s, row.start_col, cur.col, vp.tab_width);
            // A full last row leaves the end-of-line cursor in the last cell.
            let x = x.min(usize::from(vp.width).saturating_sub(1));
            return (clamp_u16(x), clamp_u16(y));
        }
    }
    (0, 0)
}

/// Buffer position under screen cell (x, y). Cells past the end of a row, or
/// rows below the last, clamp to the nearest position.
pub fn screen_to_pos(rope: &Rope, vp: &Viewport, x: u16, y: u16) -> Pos {
    let rows = RowIter::new(rope, vp, vp.top.line, vp.top.start_col).take(usize::from(vp.height));
    let Some(row) = rows.take(usize::from(y) + 1).last() else {
        return Pos::default();
    };
    let s = line_text(rope, row.line);
    let target = usize::from(x) + if vp.wrap { 0 } else { vp.hscroll };
    let cells = Cells::new(s, row.start_col, vp.tab_width).take_while(|c| c.start < row.end_col);
    let mut last = None;
    for c in cells {
        if target < c.x + c.w {
            return Pos {
                line: row.line,
                col: c.start,
            };
        }
        last = Some(c.start);
    }
    let col = if row.end_col == s.len_chars() {
        row.end_col
    } else {
        last.unwrap_or(row.start_col)
    };
    Pos {
        line: row.line,
        col,
    }
}

/// First row of `line` (walking from its start) for which `done` holds, or
/// the line's last row.
fn find_row(rope: &Rope, vp: &Viewport, line: usize, done: impl Fn(&Row) -> bool) -> Row {
    let s = line_text(rope, line);
    let mut start = 0;
    loop {
        let r = row_from(s, line, start, vp.width, vp.tab_width, vp.wrap);
        if r.end_col >= s.len_chars() || done(&r) {
            return r;
        }
        start = r.end_col;
    }
}

/// The row showing `cur` (clamped). Prefers the visible window so the common
/// case never walks from the start of a long line.
fn locate_row(rope: &Rope, vp: &Viewport, cur: Pos) -> Row {
    let rows = RowIter::new(rope, vp, vp.top.line, vp.top.start_col).take(usize::from(vp.height));
    for r in rows {
        if row_holds(rope, &r, cur) {
            return r;
        }
    }
    find_row(rope, vp, cur.line, |r| cur.col < r.end_col)
}

fn prev_row(rope: &Rope, vp: &Viewport, r: Row) -> Option<Row> {
    if vp.wrap && r.start_col > 0 {
        Some(find_row(rope, vp, r.line, |p| p.end_col >= r.start_col))
    } else if r.line > 0 {
        Some(find_row(rope, vp, r.line - 1, |_| false))
    } else {
        None
    }
}

/// Position on `row` whose start x is the closest one at or before `target`.
fn land(rope: &Rope, vp: &Viewport, row: Row, target: usize) -> Pos {
    let s = line_text(rope, row.line);
    let mut col = row.start_col;
    let mut col_x = None;
    let mut total = 0;
    let cells = Cells::new(s, row.start_col, vp.tab_width).take_while(|c| c.start < row.end_col);
    for c in cells {
        total = c.x + c.w;
        if c.x > target {
            break;
        }
        if col_x.is_none_or(|b| c.x > b) {
            col = c.start;
            col_x = Some(c.x);
        }
    }
    if row.end_col == s.len_chars() && total <= target && col_x.is_none_or(|b| total > b) {
        col = row.end_col;
    }
    Pos {
        line: row.line,
        col,
    }
}

/// Moves `rows` screen rows (wrapped rows, or lines when not wrapping),
/// clamping at the buffer ends. Returns the new position and the display x to
/// remember for the next vertical move.
pub fn move_vertical(
    rope: &Rope,
    vp: &Viewport,
    cur: Pos,
    rows: isize,
    preferred_x: Option<u16>,
) -> (Pos, u16) {
    let cur = clamp(rope, cur);
    let mut row = locate_row(rope, vp, cur);
    let x = preferred_x.map_or_else(
        || {
            x_at(
                line_text(rope, cur.line),
                row.start_col,
                cur.col,
                vp.tab_width,
            )
        },
        usize::from,
    );
    if rows > 0 {
        let mut it = RowIter::new(rope, vp, row.line, row.start_col);
        it.next();
        for _ in 0..rows.unsigned_abs() {
            match it.next() {
                Some(n) => row = n,
                None => break,
            }
        }
    } else {
        for _ in 0..rows.unsigned_abs() {
            match prev_row(rope, vp, row) {
                Some(p) => row = p,
                None => break,
            }
        }
    }
    (land(rope, vp, row, x), clamp_u16(x))
}

/// Adjusts `vp.top` (and `hscroll` when not wrapping) so `cur` is visible.
/// Nothing changes when it already is; a nearby cursor below scrolls row by
/// row; anything else (above, or a jump) puts the cursor's row at the top.
pub fn scroll_to_cursor(rope: &Rope, vp: &mut Viewport, cur: Pos) {
    let cur = clamp(rope, cur);
    let height = usize::from(vp.height).max(1);
    if !vp.wrap {
        let top = vp.top.line.min(rope.len_lines() - 1);
        let top = if cur.line < top {
            cur.line
        } else if cur.line >= top + height {
            cur.line + 1 - height
        } else {
            top
        };
        vp.top = Row {
            line: top,
            start_col: 0,
            end_col: line_text(rope, top).len_chars(),
        };
        let x = x_at(line_text(rope, cur.line), 0, cur.col, vp.tab_width);
        let width = usize::from(vp.width).max(1);
        if x < vp.hscroll {
            vp.hscroll = x;
        } else if x >= vp.hscroll + width {
            vp.hscroll = x + 1 - width;
        }
        return;
    }
    let mut ahead = RowIter::new(rope, vp, vp.top.line, vp.top.start_col);
    for _ in 0..height {
        match ahead.next() {
            Some(r) if row_holds(rope, &r, cur) => return,
            Some(_) => {}
            None => break,
        }
    }
    if (cur.line, cur.col) >= (vp.top.line, vp.top.start_col) {
        let mut tops = RowIter::new(rope, vp, vp.top.line, vp.top.start_col);
        tops.next();
        for _ in 0..height * 2 {
            let (Some(r), Some(new_top)) = (ahead.next(), tops.next()) else {
                break;
            };
            if row_holds(rope, &r, cur) {
                vp.top = new_top;
                return;
            }
        }
    }
    vp.top = find_row(rope, vp, cur.line, |r| cur.col < r.end_col);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(line: usize, start_col: usize, end_col: usize) -> Row {
        Row {
            line,
            start_col,
            end_col,
        }
    }

    #[test]
    fn short_line_one_row() {
        assert_eq!(rows_for_line("abc", 3, 10, true, 4), vec![row(3, 0, 3)]);
    }

    #[test]
    fn wraps_at_width() {
        assert_eq!(
            rows_for_line("abcdefghij", 0, 4, true, 4),
            vec![row(0, 0, 4), row(0, 4, 8), row(0, 8, 10)]
        );
        assert_eq!(rows_for_line("abcd", 0, 4, true, 4), vec![row(0, 0, 4)]);
    }

    #[test]
    fn wide_grapheme_wraps_whole() {
        assert_eq!(
            rows_for_line("abcd\u{4f60}", 0, 5, true, 4),
            vec![row(0, 0, 4), row(0, 4, 5)]
        );
    }

    #[test]
    fn empty_line_one_row() {
        assert_eq!(rows_for_line("", 7, 5, true, 4), vec![row(7, 0, 0)]);
    }

    #[test]
    fn width_one_with_cjk_no_infinite_loop() {
        assert_eq!(
            rows_for_line("\u{4f60}\u{597d}", 0, 1, true, 4),
            vec![row(0, 0, 1), row(0, 1, 2)]
        );
    }

    #[test]
    fn tab_fills_to_next_stop() {
        // "a" + tab (3 cells) fills 4; the next char wraps.
        assert_eq!(
            rows_for_line("a\tb", 0, 4, true, 4),
            vec![row(0, 0, 2), row(0, 2, 3)]
        );
    }

    #[test]
    fn no_wrap_is_one_row() {
        assert_eq!(
            rows_for_line("abcdefgh", 2, 4, false, 4),
            vec![row(2, 0, 8)]
        );
    }

    fn vp(width: u16, height: u16, wrap: bool) -> Viewport {
        Viewport {
            top: Row::default(),
            width,
            height,
            wrap,
            hscroll: 0,
            tab_width: 4,
        }
    }

    #[test]
    fn visible_rows_walks_from_top_and_stops_at_height() {
        let r = Rope::from_str("abcdef\nxy\nlast");
        let mut v = vp(4, 3, true);
        assert_eq!(
            visible_rows(&r, &v),
            vec![row(0, 0, 4), row(0, 4, 6), row(1, 0, 2)]
        );
        v.top = row(0, 4, 6);
        v.height = 10;
        assert_eq!(
            visible_rows(&r, &v),
            vec![row(0, 4, 6), row(1, 0, 2), row(2, 0, 4)]
        );
    }

    fn long_line_rope() -> Rope {
        Rope::from_str(&"x".repeat(5_000_000))
    }

    #[test]
    fn layout_long_line_only_visible_rows() {
        let r = long_line_rope();
        let mut v = vp(80, 24, true);
        v.top = row(0, 2_500_000, 0);
        let start = std::time::Instant::now();
        let rows = visible_rows(&r, &v);
        let elapsed = start.elapsed();
        assert_eq!(rows.len(), 24);
        assert_eq!(rows[0], row(0, 2_500_000, 2_500_080));
        assert!(elapsed.as_millis() < 50, "{elapsed:?}");
        println!("visible_rows long line: {elapsed:?}");
    }

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn cursor_screen_in_wrapped_line() {
        let r = Rope::from_str("abcdefgh\nij");
        let v = vp(4, 5, true);
        assert_eq!(cursor_screen(&r, &v, pos(0, 0)), (0, 0));
        assert_eq!(cursor_screen(&r, &v, pos(0, 5)), (1, 1));
        assert_eq!(cursor_screen(&r, &v, pos(1, 2)), (2, 2));
    }

    #[test]
    fn cursor_screen_long_line_is_fast() {
        let r = long_line_rope();
        let mut v = vp(80, 24, true);
        v.top = row(0, 2_500_000, 0);
        let start = std::time::Instant::now();
        let at = cursor_screen(&r, &v, pos(0, 2_500_000 + 80 * 3 + 7));
        let elapsed = start.elapsed();
        assert_eq!(at, (7, 3));
        assert!(elapsed.as_millis() < 50, "{elapsed:?}");
        println!("cursor_screen long line: {elapsed:?}");
    }

    #[test]
    fn screen_to_pos_roundtrip() {
        let text = "abcdefgh\nij\n\nk\u{4f60}\u{597d}lmn\tab";
        let r = Rope::from_str(text);
        for wrap in [true, false] {
            let v = vp(5, 12, wrap);
            for line in 0..r.len_lines() {
                let len = line_text(&r, line).len_chars();
                for col in 0..=len {
                    let p = pos(line, col);
                    let (x, y) = cursor_screen(&r, &v, p);
                    assert_eq!(screen_to_pos(&r, &v, x, y), p, "wrap={wrap} {p:?}");
                }
            }
        }
    }

    #[test]
    fn screen_to_pos_clamps_and_snaps() {
        let r = Rope::from_str("abcd\u{4f60}ef\nx");
        let v = vp(5, 4, true);
        // Past the end of a wrapped (non-final) row: last grapheme of the row.
        assert_eq!(screen_to_pos(&r, &v, 4, 0), pos(0, 3));
        // Second cell of the wide char maps to its start.
        assert_eq!(screen_to_pos(&r, &v, 1, 1), pos(0, 4));
        // Past the end of the final row of a line: end of line.
        assert_eq!(screen_to_pos(&r, &v, 4, 1), pos(0, 7));
        // Below the last row: the last row.
        assert_eq!(screen_to_pos(&r, &v, 0, 9), pos(1, 0));
        assert_eq!(screen_to_pos(&r, &v, 4, 9), pos(1, 1));
    }

    #[test]
    fn down_keeps_column_across_short_line() {
        let r = Rope::from_str("abcdef\nab\nabcdef");
        let v = vp(80, 10, true);
        let (p, pref) = move_vertical(&r, &v, pos(0, 5), 1, None);
        assert_eq!((p, pref), (pos(1, 2), 5));
        let (p, pref) = move_vertical(&r, &v, p, 1, Some(pref));
        assert_eq!((p, pref), (pos(2, 5), 5));
    }

    #[test]
    fn down_moves_within_wrapped_line() {
        let r = Rope::from_str("abcdefgh");
        let v = vp(4, 5, true);
        assert_eq!(move_vertical(&r, &v, pos(0, 1), 1, None), (pos(0, 5), 1));
        assert_eq!(move_vertical(&r, &v, pos(0, 5), -1, None), (pos(0, 1), 1));
    }

    #[test]
    fn up_at_top_stays() {
        let r = Rope::from_str("abc\ndef");
        let v = vp(10, 5, true);
        assert_eq!(move_vertical(&r, &v, pos(0, 2), -1, None), (pos(0, 2), 2));
    }

    #[test]
    fn down_at_end_stays_and_up_crosses_lines() {
        let r = Rope::from_str("abcdefgh\nxy");
        let v = vp(4, 5, true);
        assert_eq!(move_vertical(&r, &v, pos(1, 1), 1, None), (pos(1, 1), 1));
        // Up from "xy" lands on the last wrapped row of the previous line.
        assert_eq!(move_vertical(&r, &v, pos(1, 1), -1, None), (pos(0, 5), 1));
        assert_eq!(move_vertical(&r, &v, pos(1, 1), -9, None), (pos(0, 1), 1));
    }

    #[test]
    fn no_wrap_moves_by_lines() {
        let r = Rope::from_str("abcdefgh\nxy\nabcdefgh");
        let v = vp(4, 5, false);
        assert_eq!(move_vertical(&r, &v, pos(0, 6), 1, None), (pos(1, 2), 6));
        assert_eq!(move_vertical(&r, &v, pos(0, 6), 2, None), (pos(2, 6), 6));
    }

    #[test]
    fn vertical_lands_on_wide_grapheme_start() {
        let r = Rope::from_str("abcd\n\u{4f60}\u{597d}\u{4f60}");
        let v = vp(80, 5, true);
        // x = 3 is the second cell of the second wide char (cols 2..4).
        assert_eq!(move_vertical(&r, &v, pos(0, 3), 1, None), (pos(1, 1), 3));
    }

    #[test]
    fn scroll_down_row_by_row_keeps_cursor_on_last_row() {
        let r = Rope::from_str("a\nb\nc\nd\ne\nf");
        let mut v = vp(10, 3, true);
        scroll_to_cursor(&r, &mut v, pos(2, 0));
        assert_eq!(v.top, Row::default(), "visible, unchanged");
        scroll_to_cursor(&r, &mut v, pos(4, 0));
        assert_eq!(v.top.line, 2);
        assert_eq!(cursor_screen(&r, &v, pos(4, 0)), (0, 2));
    }

    #[test]
    fn scroll_up_and_jump_put_cursor_row_on_top() {
        let text = (0..100)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let r = Rope::from_str(&text);
        let mut v = vp(10, 5, true);
        scroll_to_cursor(&r, &mut v, pos(80, 1));
        assert_eq!(v.top.line, 80);
        scroll_to_cursor(&r, &mut v, pos(10, 0));
        assert_eq!(v.top.line, 10);
        assert_eq!(cursor_screen(&r, &v, pos(10, 0)), (0, 0));
    }

    #[test]
    fn scroll_within_one_wrapped_line() {
        let r = Rope::from_str("abcdefghijkl\nz");
        let mut v = vp(4, 2, true);
        scroll_to_cursor(&r, &mut v, pos(0, 9));
        assert_eq!(v.top, row(0, 4, 8));
        assert_eq!(cursor_screen(&r, &v, pos(0, 9)), (1, 1));
        scroll_to_cursor(&r, &mut v, pos(0, 1));
        assert_eq!(v.top, row(0, 0, 4));
    }

    #[test]
    fn scroll_no_wrap_adjusts_hscroll_and_top() {
        let r = Rope::from_str("abcdefghijkl\nz\ny\nx");
        let mut v = vp(4, 2, false);
        scroll_to_cursor(&r, &mut v, pos(0, 6));
        assert_eq!(v.hscroll, 3);
        assert_eq!(cursor_screen(&r, &v, pos(0, 6)), (3, 0));
        scroll_to_cursor(&r, &mut v, pos(0, 1));
        assert_eq!(v.hscroll, 1);
        scroll_to_cursor(&r, &mut v, pos(3, 0));
        assert_eq!(v.top.line, 2);
        assert_eq!(cursor_screen(&r, &v, pos(3, 0)), (0, 1));
    }

    #[test]
    fn scroll_and_vertical_on_long_line_are_local() {
        let r = long_line_rope();
        let mut v = vp(80, 24, true);
        v.top = row(0, 2_500_000, 0);
        let start = std::time::Instant::now();
        let cur = pos(0, 2_500_000 + 80 * 5 + 3);
        scroll_to_cursor(&r, &mut v, cur);
        assert_eq!(v.top.start_col, 2_500_000);
        let (p, x) = move_vertical(&r, &v, cur, 2, None);
        assert_eq!((p, x), (pos(0, 2_500_000 + 80 * 7 + 3), 3));
        assert!(start.elapsed().as_millis() < 50, "{:?}", start.elapsed());
    }
}
