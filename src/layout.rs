//! Soft-wrap layout: buffer lines to screen rows, and back.
//!
//! Columns are char indices within a line (excluding the newline). Every walk
//! goes through `Cells`, a grapheme iterator over rope chunks that starts at
//! an arbitrary column, so cost follows the rows asked for, not line length.
//! The one O(line) step is `WrapCache`, built once per line and buffer
//! version, so repeated vertical motion inside a huge wrapped line stays cheap.

use crate::cursor::{Pos, clamp, line_text, next_col};
use ropey::{Rope, RopeSlice};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// One screen row: chars `start_col..end_col` of `line` (`end_col` exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Row {
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
}

/// One grapheme: chars `start..end`, at display column `x` and `w` cells wide.
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
    /// Cells from `col`, counting display x from 0 there.
    fn new(s: RopeSlice<'a>, col: usize, tab_width: u8) -> Self {
        Self::at(s, col, 0, tab_width)
    }

    /// Cells from `col`, which sits at display x `x`.
    fn at(s: RopeSlice<'a>, col: usize, x: usize, tab_width: u8) -> Self {
        Cells {
            s,
            col: col.min(s.len_chars()),
            x,
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
        let first = self.s.char(self.col);
        let next_ascii = self.col + 1 >= self.s.len_chars() || self.s.char(self.col + 1).is_ascii();
        if first.is_ascii() && first != '\t' && next_ascii {
            // Fast path: an ASCII char followed by ASCII is its own grapheme.
            let cell = Cell {
                start: self.col,
                end: self.col + 1,
                x: self.x,
                w: 1,
            };
            self.col += 1;
            self.x += 1;
            return Some(cell);
        }
        let end = next_col(self.s, self.col);
        let g = self.s.slice(self.col..end);
        // A lone combining mark is width 0 but still occupies a col; control
        // chars other than tab (a literal CR) display as width 1.
        let w = if g.len_chars() == 1 {
            match g.char(0) {
                '\t' => self.tab - self.x % self.tab,
                c => c.width().unwrap_or(1),
            }
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
    let (mut col, mut x) = (start, 0);
    while col < len && x < width {
        if let Some(n) = ascii_run(s, col) {
            let take = n.min(width - x);
            col += take;
            x += take;
            continue;
        }
        let c = Cells::at(s, col, x, tab).next().expect("col < len");
        // A grapheme wider than the row still gets a row of its own.
        if x > 0 && x + c.w > width {
            break;
        }
        col = c.end;
        x += c.w;
    }
    Row {
        line,
        start_col: start,
        end_col: col,
    }
}

/// A row that fills the line's last row exactly to the right edge is followed
/// by an empty phantom row, so the end-of-line cursor has a cell to sit on.
fn ends_full(s: RopeSlice, row: &Row, width: u16, tab: u8, wrap: bool) -> bool {
    wrap && row.start_col < row.end_col
        && row.end_col == s.len_chars()
        && x_at(s, row.start_col, row.end_col, tab) >= usize::from(width).max(1)
}

/// Start col of the row after `row` within its line, if there is one.
fn next_start(s: RopeSlice, row: &Row, width: u16, tab: u8, wrap: bool) -> Option<usize> {
    (row.end_col < s.len_chars() || ends_full(s, row, width, tab, wrap)).then_some(row.end_col)
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
    let mut rows = Vec::new();
    let mut start = 0;
    loop {
        let r = row_from(s, line_idx, start, width, tab_width, wrap);
        rows.push(r);
        match next_start(s, &r, width, tab_width, wrap) {
            Some(n) => start = n,
            None => return rows,
        }
    }
}

/// Row start cols of one wrapped line (including a trailing phantom row).
#[derive(Debug, Clone, PartialEq, Eq)]
struct WrapCache {
    line: usize,
    width: u16,
    tab_width: u8,
    version: u64,
    starts: Vec<usize>,
}

impl WrapCache {
    fn row(&self, idx: usize, len: usize) -> Row {
        Row {
            line: self.line,
            start_col: self.starts[idx],
            end_col: self.starts.get(idx + 1).copied().unwrap_or(len),
        }
    }

    /// Index of the row holding `col` (the end-of-line col sits on the last).
    fn index_of(&self, col: usize) -> usize {
        self.starts.partition_point(|&s| s <= col) - 1
    }
}

/// Count of ASCII chars from `col` that are each their own width-1 grapheme
/// (no tab, and not followed by something that could combine with them).
fn ascii_run(s: RopeSlice, col: usize) -> Option<usize> {
    let byte = s.char_to_byte(col);
    let (chunk, chunk_start, _, _) = s.chunk_at_byte(byte);
    let rest = &chunk.as_bytes()[byte - chunk_start..];
    let mut n = if rest.is_ascii() && !rest.contains(&b'\t') {
        rest.len()
    } else {
        rest.iter().position(|&b| b == b'\t' || b >= 0x80)?
    };
    if col + n < s.len_chars() && !s.char(col + n).is_ascii() {
        n = n.checked_sub(1)?;
    }
    (n > 0).then_some(n)
}

/// Same wrapping as `row_from`, for the whole line at once, with a bulk path
/// for ASCII so a multi-megabyte line builds quickly.
fn build_starts(s: RopeSlice, width: u16, tab: u8) -> Vec<usize> {
    let len = s.len_chars();
    let width = usize::from(width).max(1);
    let mut starts = vec![0];
    let (mut col, mut x) = (0, 0);
    while col < len {
        if x >= width {
            starts.push(col);
            x = 0;
        }
        if let Some(mut left) = ascii_run(s, col) {
            while left > 0 {
                if x >= width {
                    starts.push(col);
                    x = 0;
                }
                let take = left.min(width - x);
                col += take;
                x += take;
                left -= take;
            }
            continue;
        }
        let mut c = Cells::at(s, col, x, tab).next().expect("col < len");
        if x > 0 && x + c.w > width {
            starts.push(col);
            c = Cells::new(s, col, tab).next().expect("col < len");
            x = 0;
        }
        col = c.end;
        x += c.w;
    }
    if len > 0 && x >= width {
        starts.push(len);
    }
    starts
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewport {
    pub top: Row,
    pub width: u16,
    pub height: u16,
    pub wrap: bool,
    /// No-wrap mode: display x of the leftmost visible column. `hscroll_col`
    /// is the char col at that x on `hscroll_line`; x is measured from this
    /// anchor so cost follows the distance to the cursor, not the line length.
    pub hscroll: usize,
    pub hscroll_col: usize,
    pub hscroll_line: usize,
    pub tab_width: u8,
    /// (buffer version, tab width) the anchor was computed under.
    hscroll_key: Option<(u64, u8)>,
    wrap_cache: Option<WrapCache>,
}

impl Viewport {
    /// Drops the row cache and the no-wrap anchor. Call when the buffer is
    /// replaced (open, reload): `Buffer::version` restarts at 0 then, so the
    /// version keys alone cannot tell old state from new.
    pub fn invalidate(&mut self) {
        self.wrap_cache = None;
        self.hscroll = 0;
        self.hscroll_col = 0;
        self.hscroll_line = 0;
        self.hscroll_key = None;
    }

    pub fn new(width: u16, height: u16, wrap: bool, tab_width: u8) -> Viewport {
        Viewport {
            top: Row::default(),
            width,
            height,
            wrap,
            hscroll: 0,
            hscroll_col: 0,
            hscroll_line: 0,
            tab_width,
            hscroll_key: None,
            wrap_cache: None,
        }
    }
}

/// The cached row starts of `line`, rebuilt when the key changed.
fn line_cache<'a>(rope: &Rope, vp: &'a mut Viewport, version: u64, line: usize) -> &'a WrapCache {
    let key = (line, vp.width, vp.tab_width, version);
    let hit = matches!(&vp.wrap_cache, Some(c) if (c.line, c.width, c.tab_width, c.version) == key);
    if !hit {
        vp.wrap_cache = Some(WrapCache {
            line,
            width: vp.width,
            tab_width: vp.tab_width,
            version,
            starts: build_starts(line_text(rope, line), vp.width, vp.tab_width),
        });
    }
    vp.wrap_cache.as_ref().expect("just set")
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
        if let Some(n) = next_start(s, &row, self.width, self.tab, self.wrap) {
            self.start = n;
        } else {
            self.line += 1;
            self.start = 0;
        }
        Some(row)
    }
}

/// The rows `visible_rows` would return.
fn window<'a>(rope: &'a Rope, vp: &Viewport) -> impl Iterator<Item = Row> + 'a {
    RowIter::new(rope, vp, vp.top.line, vp.top.start_col).take(usize::from(vp.height))
}

/// At most `height` rows, starting at `vp.top`.
pub fn visible_rows(rope: &Rope, vp: &Viewport) -> Vec<Row> {
    window(rope, vp).collect()
}

fn clamp_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// Whether `cur` (already clamped) is shown on `row`. The end-of-line
/// position belongs to the last row of its line.
fn row_holds(rope: &Rope, vp: &Viewport, row: &Row, cur: Pos) -> bool {
    let s = line_text(rope, row.line);
    row.line == cur.line
        && cur.col >= row.start_col
        && (cur.col < row.end_col
            || (cur.col == row.end_col
                && row.end_col == s.len_chars()
                && !ends_full(s, row, vp.width, vp.tab_width, vp.wrap)))
}

/// First grapheme boundary of the line at display x >= `target` (or the line
/// end), as (col, x). Bulk-skips ASCII so one walk over a huge line is cheap.
fn boundary_at_or_after(s: RopeSlice, target: usize, tab: u8) -> (usize, usize) {
    let len = s.len_chars();
    let (mut col, mut x) = (0, 0);
    while col < len && x < target {
        if let Some(n) = ascii_run(s, col) {
            let k = n.min(target - x);
            col += k;
            x += k;
        } else {
            let c = Cells::at(s, col, x, tab).next().expect("col < len");
            col = c.end;
            x += c.w;
        }
    }
    (col, x)
}

/// Display x of char column `col`, counted from the start of the row that
/// begins at `row_start`.
fn x_at(s: RopeSlice, row_start: usize, col: usize, tab: u8) -> usize {
    advance_x(s, row_start, 0, col, tab)
}

/// Display x reached by walking from `col` (at x `x`) over every grapheme
/// that starts before `to`.
fn advance_x(s: RopeSlice, mut col: usize, mut x: usize, to: usize, tab: u8) -> usize {
    let to = to.min(s.len_chars());
    while col < to {
        if let Some(n) = ascii_run(s, col) {
            let k = n.min(to - col);
            col += k;
            x += k;
        } else {
            let c = Cells::at(s, col, x, tab).next().expect("col < len");
            col = c.end;
            x += c.w;
        }
    }
    x
}

/// Whether the no-wrap anchor is usable on `line` for display x `target`.
fn anchored(s: RopeSlice, vp: &Viewport, line: usize, target: usize) -> bool {
    line == vp.hscroll_line && vp.hscroll_col <= s.len_chars() && target >= vp.hscroll
}

/// Absolute display x of `col` in no-wrap mode, measured from the anchor.
fn abs_x(s: RopeSlice, vp: &Viewport, line: usize, col: usize) -> usize {
    let tab = vp.tab_width;
    if col == 0 {
        0
    } else if line != vp.hscroll_line || vp.hscroll_col > s.len_chars() {
        x_at(s, 0, col, tab)
    } else if col >= vp.hscroll_col {
        advance_x(s, vp.hscroll_col, vp.hscroll, col, tab)
    } else {
        // Behind the anchor: widths only depend on the start x for tabs.
        let mut total = 0;
        for c in Cells::new(s, col, tab).take_while(|c| c.start < vp.hscroll_col) {
            if s.char(c.start) == '\t' {
                return x_at(s, 0, col, tab);
            }
            total += c.w;
        }
        vp.hscroll.saturating_sub(total)
    }
}

/// Cells covering `row`, positioned so display x matches the screen: from the
/// row start when wrapping, from the anchor when it applies, else from col 0.
fn row_cells<'a>(s: RopeSlice<'a>, vp: &Viewport, row: &Row, target: usize) -> Cells<'a> {
    if vp.wrap {
        Cells::new(s, row.start_col, vp.tab_width)
    } else if anchored(s, vp, row.line, target) {
        Cells::at(s, vp.hscroll_col, vp.hscroll, vp.tab_width)
    } else {
        Cells::new(s, 0, vp.tab_width)
    }
}

/// One grapheme as drawn: chars `start..end` of the row's line, at screen x
/// `x` (relative to the text area) and `w` cells wide.
pub(crate) struct DrawCell {
    pub start: usize,
    pub end: usize,
    pub x: usize,
    pub w: usize,
}

/// The graphemes of `row` that fit on screen, using the same tab, width and
/// grapheme rules as wrapping. In no-wrap mode a grapheme cut by either edge
/// is left out, so the caller pads the gap.
pub(crate) fn draw_cells<'a>(
    rope: &'a Rope,
    vp: &Viewport,
    row: &Row,
) -> impl Iterator<Item = DrawCell> + 'a {
    let s = line_text(rope, row.line);
    let left = if vp.wrap { 0 } else { vp.hscroll };
    let right = left + usize::from(vp.width);
    let end_col = row.end_col;
    row_cells(s, vp, row, left)
        .take_while(move |c| c.start < end_col && c.x + c.w <= right)
        .filter(move |c| c.x >= left)
        .map(move |c| DrawCell {
            start: c.start,
            end: c.end,
            x: c.x - left,
            w: c.w,
        })
}

/// Display x of `cur` on its row (absolute in no-wrap mode).
fn cursor_x(s: RopeSlice, vp: &Viewport, row_start: usize, cur: Pos) -> usize {
    if vp.wrap {
        x_at(s, row_start, cur.col, vp.tab_width)
    } else {
        abs_x(s, vp, cur.line, cur.col)
    }
}

/// Screen cell (x, y) of the cursor relative to the text area, or `None`
/// when it is outside the window `visible_rows` shows.
pub fn cursor_screen(rope: &Rope, vp: &Viewport, cur: Pos) -> Option<(u16, u16)> {
    let cur = clamp(rope, cur);
    let s = line_text(rope, cur.line);
    if !vp.wrap {
        let y = cur.line.checked_sub(vp.top.line)?;
        let x = abs_x(s, vp, cur.line, cur.col).checked_sub(vp.hscroll)?;
        if y >= usize::from(vp.height) || x >= usize::from(vp.width) {
            return None;
        }
        return Some((clamp_u16(x), clamp_u16(y)));
    }
    for (y, row) in window(rope, vp).enumerate() {
        if row_holds(rope, vp, &row, cur) {
            let x = x_at(s, row.start_col, cur.col, vp.tab_width);
            return Some((clamp_u16(x), clamp_u16(y)));
        }
    }
    None
}

/// Buffer position under screen cell (x, y). Cells past the end of a row, or
/// rows below the last, clamp to the nearest position.
pub fn screen_to_pos(rope: &Rope, vp: &Viewport, x: u16, y: u16) -> Pos {
    let Some(row) = window(rope, vp).take(usize::from(y) + 1).last() else {
        return Pos::default();
    };
    let s = line_text(rope, row.line);
    let target = usize::from(x) + if vp.wrap { 0 } else { vp.hscroll };
    let cells = row_cells(s, vp, &row, target).take_while(|c| c.start < row.end_col);
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
    let col =
        if row.end_col == s.len_chars() && !ends_full(s, &row, vp.width, vp.tab_width, vp.wrap) {
            row.end_col
        } else {
            last.unwrap_or(row.start_col)
        };
    Pos {
        line: row.line,
        col,
    }
}

/// Whole-line row used in no-wrap mode.
fn line_row(rope: &Rope, vp: &Viewport, line: usize) -> Row {
    row_from(
        line_text(rope, line),
        line,
        0,
        vp.width,
        vp.tab_width,
        false,
    )
}

/// The row showing `cur` (clamped). Prefers the visible window so the common
/// case never touches the line cache.
fn locate_row(rope: &Rope, vp: &mut Viewport, version: u64, cur: Pos) -> Row {
    if let Some(r) = window(rope, vp).find(|r| row_holds(rope, vp, r, cur)) {
        return r;
    }
    if !vp.wrap {
        return line_row(rope, vp, cur.line);
    }
    let len = line_text(rope, cur.line).len_chars();
    let c = line_cache(rope, vp, version, cur.line);
    c.row(c.index_of(cur.col), len)
}

fn prev_row(rope: &Rope, vp: &mut Viewport, version: u64, r: Row) -> Option<Row> {
    if !vp.wrap {
        return (r.line > 0).then(|| line_row(rope, vp, r.line - 1));
    }
    let rows: Vec<Row> = window(rope, vp).collect();
    if let Some(i) = rows.iter().position(|w| *w == r).filter(|&i| i > 0) {
        return Some(rows[i - 1]);
    }
    let line = if r.start_col > 0 {
        r.line
    } else if r.line > 0 {
        r.line - 1
    } else {
        return None;
    };
    let len = line_text(rope, line).len_chars();
    let c = line_cache(rope, vp, version, line);
    let idx = if r.start_col > 0 {
        c.index_of(r.start_col) - 1
    } else {
        c.starts.len() - 1
    };
    Some(c.row(idx, len))
}

/// Position on `row` whose start x is the closest one at or before `target`.
fn land(rope: &Rope, vp: &Viewport, row: Row, target: usize) -> Pos {
    let s = line_text(rope, row.line);
    let mut col = row.start_col;
    let mut col_x = None;
    let mut total = 0;
    let cells = row_cells(s, vp, &row, target).take_while(|c| c.start < row.end_col);
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
    if row.end_col == s.len_chars()
        && !ends_full(s, &row, vp.width, vp.tab_width, vp.wrap)
        && total <= target
        && col_x.is_none_or(|b| total > b)
    {
        col = row.end_col;
    }
    Pos {
        line: row.line,
        col,
    }
}

/// Moves `rows` screen rows (wrapped rows, or lines when not wrapping),
/// clamping at the buffer ends. Returns the new position and the display x to
/// remember for the next vertical move. `version` is `Buffer::version`.
pub fn move_vertical(
    rope: &Rope,
    vp: &mut Viewport,
    cur: Pos,
    rows: isize,
    preferred_x: Option<u16>,
    version: u64,
) -> (Pos, u16) {
    let cur = clamp(rope, cur);
    let mut row = locate_row(rope, vp, version, cur);
    let x = preferred_x.map_or_else(
        || cursor_x(line_text(rope, cur.line), vp, row.start_col, cur),
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
            match prev_row(rope, vp, version, row) {
                Some(p) => row = p,
                None => break,
            }
        }
    }
    (land(rope, vp, row, x), clamp_u16(x))
}

/// Moves the top of the window `rows` screen rows (negative is up), stopping
/// at the first and last row of the buffer. The cursor is not considered.
pub fn scroll_rows(rope: &Rope, vp: &mut Viewport, rows: isize, version: u64) {
    let mut top = locate_top(rope, vp);
    if rows > 0 {
        let mut it = RowIter::new(rope, vp, top.line, top.start_col);
        it.next();
        for _ in 0..rows {
            match it.next() {
                Some(n) => top = n,
                None => break,
            }
        }
    } else {
        for _ in 0..rows.unsigned_abs() {
            match prev_row(rope, vp, version, top) {
                Some(p) => top = p,
                None => break,
            }
        }
    }
    vp.top = top;
}

/// `vp.top` as a real row of the buffer (its `end_col` may be stale).
fn locate_top(rope: &Rope, vp: &Viewport) -> Row {
    RowIter::new(rope, vp, vp.top.line, vp.top.start_col)
        .next()
        .unwrap_or(vp.top)
}

/// Adjusts `vp.top` (and the horizontal anchor when not wrapping) so `cur` is
/// visible. Nothing changes when it already is; a nearby cursor below scrolls
/// row by row; anything else puts the cursor's row at the top.
pub fn scroll_to_cursor(rope: &Rope, vp: &mut Viewport, cur: Pos, version: u64) {
    let cur = clamp(rope, cur);
    let height = usize::from(vp.height).max(1);
    if !vp.wrap {
        scroll_nowrap(rope, vp, cur, height, version);
        return;
    }
    vp.hscroll_key = None; // wrap mode edits are invisible to the anchor
    let mut ahead = RowIter::new(rope, vp, vp.top.line, vp.top.start_col);
    for _ in 0..height {
        match ahead.next() {
            Some(r) if row_holds(rope, vp, &r, cur) => return,
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
            if row_holds(rope, vp, &r, cur) {
                vp.top = new_top;
                return;
            }
        }
    }
    let len = line_text(rope, cur.line).len_chars();
    let c = line_cache(rope, vp, version, cur.line);
    vp.top = c.row(c.index_of(cur.col), len);
}

fn scroll_nowrap(rope: &Rope, vp: &mut Viewport, cur: Pos, height: usize, version: u64) {
    let top = vp.top.line.min(rope.len_lines() - 1);
    let top = if cur.line < top {
        cur.line
    } else if cur.line >= top + height {
        cur.line + 1 - height
    } else {
        top
    };
    vp.top = line_row(rope, vp, top);
    let s = line_text(rope, cur.line);
    let len = s.len_chars();
    let tab = vp.tab_width;
    let key = Some((version, tab));
    let same_line = vp.hscroll_line == cur.line && vp.hscroll_col <= len;
    if same_line && vp.hscroll_key != key {
        // An edit at or right of the anchor leaves it valid; a tab width
        // change or an edit possibly before it does not.
        let edit_right = vp.hscroll_key.is_some_and(|(_, t)| t == tab) && cur.col > vp.hscroll_col;
        vp.hscroll_key = if edit_right { key } else { None };
    }
    if !same_line || vp.hscroll_key.is_none() {
        // Re-anchor at the same display offset, found from the line start:
        // the first grapheme boundary at or after it.
        let (col, x) = boundary_at_or_after(s, vp.hscroll, tab);
        (vp.hscroll_line, vp.hscroll_col, vp.hscroll) = (cur.line, col, x);
        vp.hscroll_key = key;
    }
    let x = abs_x(s, vp, cur.line, cur.col);
    let width = usize::from(vp.width).max(1);
    if x < vp.hscroll {
        (vp.hscroll_col, vp.hscroll) = (cur.col, x);
    } else if x >= vp.hscroll + width {
        let want = x + 1 - width;
        let (col, x) = Cells::at(s, vp.hscroll_col, vp.hscroll, tab)
            .find(|c| c.x >= want)
            .map_or((cur.col, x), |c| (c.start, c.x));
        (vp.hscroll_col, vp.hscroll) = (col, x);
    }
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
        assert_eq!(rows_for_line("abc", 0, 4, true, 4), vec![row(0, 0, 3)]);
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
            vec![row(0, 0, 1), row(0, 1, 2), row(0, 2, 2)]
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
    fn full_row_eol_cursor_on_next_row() {
        assert_eq!(
            rows_for_line("abcd", 0, 4, true, 4),
            vec![row(0, 0, 4), row(0, 4, 4)]
        );
        let r = Rope::from_str("abcd");
        let v = vp(4, 5, true);
        assert_eq!(cursor_screen(&r, &v, pos(0, 4)).unwrap(), (0, 1));
        assert_eq!(cursor_screen(&r, &v, pos(0, 3)).unwrap(), (3, 0));
    }

    #[test]
    fn tab_filling_row_gets_phantom() {
        assert_eq!(
            rows_for_line("a\t", 0, 4, true, 4),
            vec![row(0, 0, 2), row(0, 2, 2)]
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
        Viewport::new(width, height, wrap, 4)
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
            vec![row(0, 4, 6), row(1, 0, 2), row(2, 0, 4), row(2, 4, 4)]
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
        assert_eq!(cursor_screen(&r, &v, pos(0, 0)).unwrap(), (0, 0));
        assert_eq!(cursor_screen(&r, &v, pos(0, 5)).unwrap(), (1, 1));
        assert_eq!(cursor_screen(&r, &v, pos(1, 2)).unwrap(), (2, 3));
    }

    #[test]
    fn cursor_screen_long_line_is_fast() {
        let r = long_line_rope();
        let mut v = vp(80, 24, true);
        v.top = row(0, 2_500_000, 0);
        let start = std::time::Instant::now();
        let at = cursor_screen(&r, &v, pos(0, 2_500_000 + 80 * 3 + 7)).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(at, (7, 3));
        assert!(elapsed.as_millis() < 50, "{elapsed:?}");
        println!("cursor_screen long line: {elapsed:?}");
    }

    #[test]
    fn screen_to_pos_roundtrip() {
        let text = "abcdefgh\nij\n\nk\u{4f60}\u{597d}lmn\ta";
        let r = Rope::from_str(text);
        for wrap in [true, false] {
            let v = vp(5, 12, wrap);
            for line in 0..r.len_lines() {
                let len = line_text(&r, line).len_chars();
                for col in 0..=len {
                    let p = pos(line, col);
                    let at = cursor_screen(&r, &v, p);
                    // Unscrolled no-wrap columns past the width are off screen.
                    assert!(!wrap || at.is_some(), "{p:?}");
                    let Some((x, y)) = at else { continue };
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
        let mut v = vp(80, 10, true);
        let (p, pref) = move_vertical(&r, &mut v, pos(0, 5), 1, None, 0);
        assert_eq!((p, pref), (pos(1, 2), 5));
        let (p, pref) = move_vertical(&r, &mut v, p, 1, Some(pref), 0);
        assert_eq!((p, pref), (pos(2, 5), 5));
    }

    #[test]
    fn down_moves_within_wrapped_line() {
        let r = Rope::from_str("abcdefgh");
        let mut v = vp(4, 5, true);
        assert_eq!(
            move_vertical(&r, &mut v, pos(0, 1), 1, None, 0),
            (pos(0, 5), 1)
        );
        assert_eq!(
            move_vertical(&r, &mut v, pos(0, 5), -1, None, 0),
            (pos(0, 1), 1)
        );
    }

    #[test]
    fn up_at_top_stays() {
        let r = Rope::from_str("abc\ndef");
        let mut v = vp(10, 5, true);
        assert_eq!(
            move_vertical(&r, &mut v, pos(0, 2), -1, None, 0),
            (pos(0, 2), 2)
        );
    }

    #[test]
    fn down_at_end_stays_and_up_crosses_lines() {
        let r = Rope::from_str("abcdefgh\nxy");
        let mut v = vp(4, 5, true);
        assert_eq!(
            move_vertical(&r, &mut v, pos(1, 1), 1, None, 0),
            (pos(1, 1), 1)
        );
        // Up from "xy" lands on the last wrapped row of the previous line.
        assert_eq!(
            move_vertical(&r, &mut v, pos(1, 1), -1, None, 0),
            (pos(0, 8), 1)
        );
        assert_eq!(
            move_vertical(&r, &mut v, pos(1, 1), -9, None, 0),
            (pos(0, 1), 1)
        );
    }

    #[test]
    fn no_wrap_moves_by_lines() {
        let r = Rope::from_str("abcdefgh\nxy\nabcdefgh");
        let mut v = vp(4, 5, false);
        assert_eq!(
            move_vertical(&r, &mut v, pos(0, 6), 1, None, 0),
            (pos(1, 2), 6)
        );
        assert_eq!(
            move_vertical(&r, &mut v, pos(0, 6), 2, None, 0),
            (pos(2, 6), 6)
        );
    }

    #[test]
    fn vertical_lands_on_wide_grapheme_start() {
        let r = Rope::from_str("abcd\n\u{4f60}\u{597d}\u{4f60}");
        let mut v = vp(80, 5, true);
        // x = 3 is the second cell of the second wide char (cols 2..4).
        assert_eq!(
            move_vertical(&r, &mut v, pos(0, 3), 1, None, 0),
            (pos(1, 1), 3)
        );
    }

    #[test]
    fn scroll_down_row_by_row_keeps_cursor_on_last_row() {
        let r = Rope::from_str("a\nb\nc\nd\ne\nf");
        let mut v = vp(10, 3, true);
        scroll_to_cursor(&r, &mut v, pos(2, 0), 0);
        assert_eq!(v.top, Row::default(), "visible, unchanged");
        scroll_to_cursor(&r, &mut v, pos(4, 0), 0);
        assert_eq!(v.top.line, 2);
        assert_eq!(cursor_screen(&r, &v, pos(4, 0)).unwrap(), (0, 2));
    }

    #[test]
    fn scroll_up_and_jump_put_cursor_row_on_top() {
        let text = (0..100)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let r = Rope::from_str(&text);
        let mut v = vp(10, 5, true);
        scroll_to_cursor(&r, &mut v, pos(80, 1), 0);
        assert_eq!(v.top.line, 80);
        scroll_to_cursor(&r, &mut v, pos(10, 0), 0);
        assert_eq!(v.top.line, 10);
        assert_eq!(cursor_screen(&r, &v, pos(10, 0)).unwrap(), (0, 0));
    }

    #[test]
    fn scroll_within_one_wrapped_line() {
        let r = Rope::from_str("abcdefghijkl\nz");
        let mut v = vp(4, 2, true);
        scroll_to_cursor(&r, &mut v, pos(0, 9), 0);
        assert_eq!(v.top, row(0, 4, 8));
        assert_eq!(cursor_screen(&r, &v, pos(0, 9)).unwrap(), (1, 1));
        scroll_to_cursor(&r, &mut v, pos(0, 1), 0);
        assert_eq!(v.top, row(0, 0, 4));
    }

    #[test]
    fn scroll_no_wrap_adjusts_hscroll_and_top() {
        let r = Rope::from_str("abcdefghijkl\nz\ny\nx");
        let mut v = vp(4, 2, false);
        scroll_to_cursor(&r, &mut v, pos(0, 6), 0);
        assert_eq!(v.hscroll, 3);
        assert_eq!(cursor_screen(&r, &v, pos(0, 6)).unwrap(), (3, 0));
        scroll_to_cursor(&r, &mut v, pos(0, 1), 0);
        assert_eq!(v.hscroll, 1);
        scroll_to_cursor(&r, &mut v, pos(3, 0), 0);
        assert_eq!(v.top.line, 2);
        assert_eq!(cursor_screen(&r, &v, pos(3, 0)).unwrap(), (0, 1));
    }

    #[test]
    fn scroll_and_vertical_on_long_line_are_local() {
        let r = long_line_rope();
        let mut v = vp(80, 24, true);
        v.top = row(0, 2_500_000, 0);
        let start = std::time::Instant::now();
        let cur = pos(0, 2_500_000 + 80 * 5 + 3);
        scroll_to_cursor(&r, &mut v, cur, 0);
        assert_eq!(v.top.start_col, 2_500_000);
        let (p, x) = move_vertical(&r, &mut v, cur, 2, None, 0);
        assert_eq!((p, x), (pos(0, 2_500_000 + 80 * 7 + 3), 3));
        // Up inside the visible window never builds the line cache.
        let (p, x) = move_vertical(&r, &mut v, p, -3, None, 0);
        assert_eq!((p, x), (pos(0, 2_500_000 + 80 * 4 + 3), 3));
        assert!(v.wrap_cache.is_none());
        assert!(start.elapsed().as_millis() < 50, "{:?}", start.elapsed());
    }

    #[test]
    fn repeated_up_at_top_of_long_line_is_fast() {
        let r = long_line_rope();
        let mut v = vp(80, 24, true);
        v.top = row(0, 2_500_000, 0);
        let mut cur = pos(0, 2_500_000 + 3);
        let start = std::time::Instant::now();
        for _ in 0..50 {
            cur = move_vertical(&r, &mut v, cur, -1, None, 0).0;
            scroll_to_cursor(&r, &mut v, cur, 0);
        }
        let elapsed = start.elapsed();
        assert_eq!(cur, pos(0, 2_500_000 + 3 - 50 * 80));
        assert_eq!(v.top.start_col, 2_500_000 - 50 * 80);
        assert!(elapsed.as_millis() < 200, "{elapsed:?}");
        println!("50 x Up at top of long line: {elapsed:?}");
    }

    #[test]
    fn nowrap_long_line_cursor_is_fast() {
        let r = long_line_rope();
        let mut v = vp(80, 24, false);
        v.hscroll_line = 0;
        v.hscroll_col = 2_500_000;
        v.hscroll = 2_500_000;
        let cur = pos(0, 2_500_010);
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            scroll_to_cursor(&r, &mut v, cur, 0);
            assert_eq!(cursor_screen(&r, &v, cur).unwrap(), (10, 0));
        }
        let elapsed = start.elapsed();
        assert_eq!(v.hscroll_col, 2_500_000);
        assert!(elapsed.as_millis() < 200, "{elapsed:?}");
        println!("1000 x nowrap cursor_screen+scroll: {elapsed:?}");
    }

    #[test]
    fn nowrap_anchor_follows_cursor_left_and_across_lines() {
        let r = Rope::from_str("abcdefghijkl\nxyz\n\tabcdefghij");
        let mut v = vp(4, 5, false);
        scroll_to_cursor(&r, &mut v, pos(0, 10), 0);
        assert_eq!((v.hscroll, v.hscroll_col), (7, 7));
        scroll_to_cursor(&r, &mut v, pos(0, 5), 0);
        assert_eq!((v.hscroll, v.hscroll_col), (5, 5));
        // A new line keeps the display offset where the line is long enough.
        scroll_to_cursor(&r, &mut v, pos(2, 9), 0);
        assert_eq!(cursor_screen(&r, &v, pos(2, 9)).unwrap(), (3, 2));
        assert_eq!(screen_to_pos(&r, &v, 3, 2), pos(2, 9));
    }

    #[test]
    fn cache_starts_match_row_walk() {
        let text = "ab\tc\u{4f60}\u{597d}e\u{301}xyz\r\u{1F468}\u{200D}\u{1F469}wxyz0123456789\tq";
        let rope = Rope::from_str(text);
        for width in 1..9u16 {
            let rows = rows_for_line(text, 0, width, true, 4);
            let starts: Vec<usize> = rows.iter().map(|r| r.start_col).collect();
            assert_eq!(
                build_starts(rope.slice(..), width, 4),
                starts,
                "width {width}"
            );
        }
    }

    #[test]
    fn jump_uses_fresh_cache_after_edit() {
        let mut r = Rope::from_str(&"a".repeat(40));
        let mut v = vp(4, 1, true);
        scroll_to_cursor(&r, &mut v, pos(0, 36), 0);
        assert_eq!(v.top, row(0, 36, 40));
        // A wide char at the front shifts every row start.
        r.insert(0, "\u{4f60}");
        scroll_to_cursor(&r, &mut v, pos(0, 1), 1);
        scroll_to_cursor(&r, &mut v, pos(0, 36), 1);
        assert_eq!(v.top, row(0, 35, 39));
    }

    #[test]
    fn nowrap_anchor_survives_backspace_before_it() {
        let mut r = Rope::from_str("xxxxxx\u{4f60}abcdefghijklmnop\n0123456789abcdefghijklmnop");
        let mut v = vp(4, 5, false);
        scroll_to_cursor(&r, &mut v, pos(0, 10), 0);
        assert_eq!((v.hscroll, v.hscroll_col), (8, 7));
        r.remove(6..7);
        scroll_to_cursor(&r, &mut v, pos(0, 6), 1);
        let s = line_text(&r, 0);
        let scratch = x_at(s, 0, 6, 4);
        assert_eq!(scratch, 6);
        assert_eq!(
            usize::from(cursor_screen(&r, &v, pos(0, 6)).unwrap().0),
            scratch - v.hscroll
        );
        // The anchor line and another line agree on x for the same column.
        let at0 = screen_to_pos(&r, &v, 1, 0);
        let at1 = screen_to_pos(&r, &v, 1, 1);
        assert_eq!((at0.col, at1.col), (7, 7));
    }

    #[test]
    fn nowrap_anchor_reset_on_tab_width_change() {
        let r = Rope::from_str("\tabcdefghijklmnop");
        let mut v = vp(4, 5, false);
        scroll_to_cursor(&r, &mut v, pos(0, 9), 0);
        v.tab_width = 8;
        scroll_to_cursor(&r, &mut v, pos(0, 9), 0);
        let scratch = x_at(line_text(&r, 0), 0, 9, 8);
        assert_eq!(
            usize::from(cursor_screen(&r, &v, pos(0, 9)).unwrap().0),
            scratch - v.hscroll
        );
    }

    #[test]
    fn nowrap_edit_right_of_anchor_keeps_anchor() {
        let mut r = Rope::from_str("abcdefghijkl");
        let mut v = vp(4, 5, false);
        scroll_to_cursor(&r, &mut v, pos(0, 10), 0);
        let anchor = (v.hscroll, v.hscroll_col);
        r.insert(10, "z");
        scroll_to_cursor(&r, &mut v, pos(0, 11), 1);
        assert!(v.hscroll_col >= anchor.1);
        assert_eq!(cursor_screen(&r, &v, pos(0, 11)).unwrap().0, 3);
    }

    #[test]
    fn invalidate_clears_caches() {
        let r = Rope::from_str(&"a".repeat(40));
        let mut v = vp(4, 1, true);
        scroll_to_cursor(&r, &mut v, pos(0, 36), 0);
        assert!(v.wrap_cache.is_some());
        v.hscroll = 5;
        v.hscroll_col = 5;
        v.invalidate();
        assert!(v.wrap_cache.is_none());
        assert_eq!((v.hscroll, v.hscroll_col, v.hscroll_line), (0, 0, 0));
    }

    fn drawn(rope: &Rope, vp: &Viewport) -> Vec<Vec<(usize, usize, usize)>> {
        visible_rows(rope, vp)
            .iter()
            .map(|r| {
                draw_cells(rope, vp, r)
                    .map(|c| (c.start, c.x, c.w))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn draw_cells_match_row_layout() {
        // tab to the next stop, wide char wraps whole, combining mark is width 0
        let rope = Rope::from_str("a\tb\u{4f60}e\u{301}\r");
        let vp = Viewport::new(6, 5, true, 4);
        assert_eq!(
            drawn(&rope, &vp),
            vec![
                vec![(0, 0, 1), (1, 1, 3), (2, 4, 1)],
                vec![(3, 0, 2), (4, 2, 1), (6, 3, 1)],
            ]
        );
    }

    #[test]
    fn draw_cells_nowrap_clip_to_screen() {
        let rope = Rope::from_str("\u{4f60}abcdefgh");
        let mut vp = Viewport::new(4, 5, false, 4);
        let cur = Pos { line: 0, col: 7 };
        scroll_to_cursor(&rope, &mut vp, cur, 0);
        assert_eq!(vp.hscroll, 5);
        assert_eq!(
            drawn(&rope, &vp),
            vec![vec![(4, 0, 1), (5, 1, 1), (6, 2, 1), (7, 3, 1)]]
        );
        vp.hscroll = 1;
        vp.hscroll_line = 9;
        // the wide char straddles the left edge and is skipped
        assert_eq!(
            drawn(&rope, &vp),
            vec![vec![(1, 1, 1), (2, 2, 1), (3, 3, 1)]]
        );
    }

    #[test]
    fn cursor_screen_none_when_above_top() {
        let r = Rope::from_str("a\nb\nc\nd\nabcdefgh");
        for wrap in [true, false] {
            let mut v = vp(4, 2, wrap);
            v.top = Row {
                line: 2,
                start_col: 0,
                end_col: 1,
            };
            assert_eq!(cursor_screen(&r, &v, pos(0, 0)), None, "wrap={wrap}");
            assert_eq!(cursor_screen(&r, &v, pos(1, 1)), None, "wrap={wrap}");
            assert_eq!(
                cursor_screen(&r, &v, pos(2, 0)),
                Some((0, 0)),
                "wrap={wrap}"
            );
        }
        // Wrapped line scrolled past its first row.
        let mut v = vp(4, 2, true);
        v.top = row(4, 4, 8);
        assert_eq!(cursor_screen(&r, &v, pos(4, 1)), None);
        assert_eq!(cursor_screen(&r, &v, pos(4, 5)), Some((1, 0)));
    }

    #[test]
    fn cursor_screen_none_when_below_window() {
        let r = Rope::from_str("a\nb\nc\nd\nabcdefgh");
        for wrap in [true, false] {
            let v = vp(4, 2, wrap);
            assert_eq!(
                cursor_screen(&r, &v, pos(1, 0)),
                Some((0, 1)),
                "wrap={wrap}"
            );
            assert_eq!(cursor_screen(&r, &v, pos(2, 0)), None, "wrap={wrap}");
            assert_eq!(cursor_screen(&r, &v, pos(4, 6)), None, "wrap={wrap}");
        }
        // No-wrap: x outside the horizontal window.
        let mut v = vp(4, 5, false);
        v.hscroll = 3;
        v.hscroll_line = 4;
        v.hscroll_col = 3;
        assert_eq!(cursor_screen(&r, &v, pos(4, 1)), None);
        assert_eq!(cursor_screen(&r, &v, pos(4, 8)), None);
        assert_eq!(cursor_screen(&r, &v, pos(4, 5)), Some((2, 4)));
    }
}
