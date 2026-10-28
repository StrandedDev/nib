use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::terminal;

use crate::buffer::Buffer;
use crate::clipboard::{Clip, Osc52};
use crate::commands::CommandId;
use crate::cursor::{self, Pos};
use crate::edit::Editor;
use crate::fileio;
use crate::files::{FileFlow, Then, file_name};
use crate::find::FindSession;
use crate::indent::Indent;
use crate::keymap::Keymap;
use crate::layout::{self, Viewport};
use crate::palette::Palette;
use crate::prompt::{Prompt, PromptResult};
use crate::recovery::Recovery;
use crate::render::{self, AppView, PaletteView};
use crate::terminal::Guard;

pub struct App {
    pub(crate) editor: Editor,
    pub(crate) path: Option<PathBuf>,
    pub(crate) viewport: Viewport,
    /// Terminal size (columns, rows).
    pub(crate) screen: (u16, u16),
    pub(crate) keymap: Keymap,
    /// Never saved: no file on disk yet.
    pub(crate) is_new: bool,
    pub(crate) disk_mtime: Option<SystemTime>,
    pub(crate) read_only: bool,
    pub(crate) prompt: Option<(Prompt, PromptPurpose)>,
    pub(crate) find: Option<FindSession>,
    pub(crate) palette: Option<Palette>,
    /// Query of the last find session, repeated by Find Next / Previous.
    pub(crate) last_search: Option<String>,
    pub(crate) status: Option<(String, Instant)>,
    /// Time and cell of the last left-button press, for double-click.
    pub(crate) last_click: Option<(Instant, u16, u16)>,
    pub(crate) clip: Box<dyn Clip>,
    pub(crate) quit: bool,
    pub(crate) line_numbers: bool,
    pub(crate) recovery: Recovery,
}

/// What the open prompt is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptPurpose {
    File(FileFlow),
    GotoLine,
}

const TAB_WIDTH: u8 = 4;
const STATUS_FADE: Duration = Duration::from_secs(4);
const WHEEL_ROWS: isize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const POLL: Duration = Duration::from_millis(250);

impl App {
    pub(crate) fn set_indent(&mut self, indent: Indent) {
        self.editor.indent = indent;
        self.viewport.tab_width = match indent {
            Indent::Spaces(n) => n,
            Indent::Tabs => TAB_WIDTH,
        };
    }

    pub fn with_clipboard(mut self, clip: Box<dyn Clip>) -> App {
        self.clip = clip;
        self
    }

    /// The terminal loop. The only place that takes over the terminal; the
    /// guard restores it on every return.
    pub fn run(&mut self) -> Result<()> {
        let r = self.run_loop();
        if r.is_err() {
            self.on_fatal_error();
        }
        r
    }

    /// Before an error ends nib: keep a dirty buffer in the recovery file.
    /// Best effort; the error being reported matters more.
    pub(crate) fn on_fatal_error(&mut self) {
        let _ = self.recovery.flush(&self.editor.buf, self.path.as_deref());
    }

    fn run_loop(&mut self) -> Result<()> {
        let _guard = Guard::enter()?;
        let (w, h) = terminal::size()?;
        self.handle_event(Event::Resize(w, h));
        let mut out = io::BufWriter::with_capacity(1 << 16, io::stdout().lock());
        let mut redraw = true;
        while !self.quit {
            if redraw {
                render::draw(&mut out, &self.view())?;
                // After the frame, so it cannot land in the middle of one.
                if let Some(seq) = self.clip.take_pending() {
                    out.write_all(seq.as_bytes())?;
                    out.flush()?;
                }
            }
            let mut changed = false;
            if event::poll(POLL)? {
                let ev = event::read()?;
                // Mouse capture reports every motion; nothing uses it.
                changed = !matches!(&ev, Event::Mouse(m) if m.kind == MouseEventKind::Moved);
                self.handle_event(ev);
            }
            let now = Instant::now();
            let ticked = self.tick(now);
            redraw = self.expire_status(now) || changed || ticked;
        }
        out.flush()?;
        Ok(())
    }

    /// Timed work for the loop: the recovery file. Returns whether the
    /// status line changed.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        let msg = self
            .recovery
            .tick(now, &self.editor.buf, self.path.as_deref());
        let changed = msg.is_some();
        self.status_from(msg);
        changed
    }

    fn view(&self) -> AppView<'_> {
        AppView {
            editor: &self.editor,
            viewport: &self.viewport,
            screen: self.screen,
            gutter: self.gutter(),
            name: self
                .path
                .as_deref()
                .map_or("[No Name]".to_string(), file_name),
            state: title_state(self),
            status: self.status_text(),
            prompt: (self.find.as_ref().map(FindSession::prompt))
                .or(self.prompt.as_ref().map(|(p, _)| p)),
            find: self.find.as_ref(),
            palette: self.palette.as_ref().map(|p| PaletteView {
                query: &p.query,
                selected: p.selected,
                items: p.items(&self.keymap),
            }),
        }
    }

    pub(crate) fn dispatch(&mut self, id: CommandId) {
        use CommandId as C;
        let rope = &self.editor.buf.rope;
        let cur = self.editor.cursor;
        let to = match id {
            C::Left | C::SelLeft => Some(cursor::left(rope, cur)),
            C::Right | C::SelRight => Some(cursor::right(rope, cur)),
            C::WordLeft | C::SelWordLeft => Some(cursor::word_left(rope, cur)),
            C::WordRight | C::SelWordRight => Some(cursor::word_right(rope, cur)),
            C::Home | C::SelHome => Some(cursor::line_start(rope, cur)),
            C::End | C::SelEnd => Some(cursor::line_end(rope, cur)),
            C::FileStart => Some(cursor::file_start(rope)),
            C::FileEnd => Some(cursor::file_end(rope)),
            _ => None,
        };
        if let Some(p) = to {
            let select = matches!(
                id,
                C::SelLeft
                    | C::SelRight
                    | C::SelWordLeft
                    | C::SelWordRight
                    | C::SelHome
                    | C::SelEnd
            );
            match id {
                C::Left if self.editor.selection().is_some() => self.editor.collapse(true),
                C::Right if self.editor.selection().is_some() => self.editor.collapse(false),
                _ => self.editor.move_to(p, select),
            }
            return;
        }
        let page = self.viewport.height.max(1) as isize;
        match id {
            C::Up => self.vertical(-1, false),
            C::SelUp => self.vertical(-1, true),
            C::Down => self.vertical(1, false),
            C::SelDown => self.vertical(1, true),
            C::PageUp => self.vertical(-page, false),
            C::PageDown => self.vertical(page, false),
            C::SelectAll => self.editor.select_all(),
            C::Backspace => self.editor.backspace(),
            C::Delete => self.editor.delete(),
            C::DeleteWordBack => self.editor.delete_word_back(),
            C::DeleteWordFwd => self.editor.delete_word_fwd(),
            C::Newline => self.editor.newline(),
            C::Tab => self.editor.tab(),
            C::Undo | C::Redo => {
                if id == C::Undo {
                    self.editor.undo();
                } else {
                    self.editor.redo();
                }
                // Text left of a no-wrap anchor may have changed under the cursor.
                self.viewport.invalidate();
            }
            C::Find | C::Replace => {
                self.find = Some(FindSession::new(&self.editor, id == C::Replace))
            }
            C::Quit => self.quit(),
            C::Copy => self.copy(),
            C::Cut => self.cut(),
            C::Paste => self.paste(),
            C::Save => {
                self.save(Then::Stay);
            }
            C::Open => self.ask_open(),
            C::SaveAs => self.save_as(),
            C::ToggleLineNumbers => {
                self.line_numbers = !self.line_numbers;
                self.fit_width();
            }
            C::ToggleWrap => {
                self.viewport.wrap = !self.viewport.wrap;
                self.relayout();
            }
            C::GotoLine => self.prompt = Some((Prompt::text("Line: "), PromptPurpose::GotoLine)),
            C::Palette => self.palette = Some(Palette::default()),
            // Moves were handled above.
            C::Left
            | C::Right
            | C::WordLeft
            | C::WordRight
            | C::Home
            | C::End
            | C::FileStart
            | C::FileEnd
            | C::SelLeft
            | C::SelRight
            | C::SelWordLeft
            | C::SelWordRight
            | C::SelHome
            | C::SelEnd => {}
            C::FindNext | C::FindPrev => self.repeat_find(id == C::FindNext),
        }
    }

    /// After a change of text width or wrap mode. Old row starts and the
    /// no-wrap anchor mean nothing now; the line start always is a row
    /// start, and scrolling to the cursor finds the rest.
    fn relayout(&mut self) {
        let vp = &mut self.viewport;
        vp.invalidate();
        vp.top = layout::Row {
            line: vp.top.line,
            start_col: 0,
            end_col: 0,
        };
        let e = &self.editor;
        layout::scroll_to_cursor(&e.buf.rope, vp, e.cursor, e.buf.version);
    }

    /// Columns taken by line numbers: digits of the last line number and a
    /// space, or none when they are off.
    pub(crate) fn gutter(&self) -> u16 {
        if self.line_numbers {
            render::gutter_width(self.editor.buf.rope.len_lines()).min(self.screen.0)
        } else {
            0
        }
    }

    /// Gives the text area what the gutter leaves. Cheap when nothing changed.
    fn fit_width(&mut self) {
        let w = self.screen.0.saturating_sub(self.gutter());
        if self.viewport.width != w {
            self.viewport.width = w;
            self.relayout();
        }
    }

    fn copy(&mut self) {
        match self.editor.selected_text() {
            Some(text) => self.clip_set(&text),
            None => self.set_status("Nothing selected"),
        }
    }

    fn cut(&mut self) {
        let text = self.editor.cut();
        self.clip_set(&text);
    }

    // Many terminals intercept Ctrl+V and send a bracketed paste instead.
    fn paste(&mut self) {
        if let Some(text) = self.clip.get() {
            self.editor.insert_str(&text); // normalises CRLF
        }
    }

    fn clip_set(&mut self, text: &str) {
        self.clip.set(text);
        if let Some(msg) = self.clip.take_notice() {
            self.set_status(msg);
        }
    }

    fn vertical(&mut self, rows: isize, select: bool) {
        let e = &mut self.editor;
        let preferred = e
            .preferred_col
            .map(|c| u16::try_from(c).unwrap_or(u16::MAX));
        let (p, x) = layout::move_vertical(
            &e.buf.rope,
            &mut self.viewport,
            e.cursor,
            rows,
            preferred,
            e.buf.version,
        );
        e.move_to(p, select);
        e.preferred_col = Some(usize::from(x));
    }

    pub(crate) fn set_status(&mut self, msg: &str) {
        self.status = Some((msg.to_string(), Instant::now()));
    }

    /// Drops a status message once it is `STATUS_FADE` old. Returns whether
    /// one was dropped, so the caller knows to redraw.
    fn expire_status(&mut self, now: Instant) -> bool {
        let old = matches!(&self.status, Some((_, t)) if now.saturating_duration_since(*t) >= STATUS_FADE);
        if old {
            self.status = None;
        }
        old
    }

    fn status_text(&self) -> Option<&str> {
        self.status.as_ref().map(|(s, _)| s.as_str())
    }

    fn prompt_key(&mut self, key: &KeyEvent) {
        let Some((prompt, _)) = &mut self.prompt else {
            return;
        };
        let result = prompt.handle_key(key);
        if result == PromptResult::Pending {
            return;
        }
        let Some((_, purpose)) = self.prompt.take() else {
            return;
        };
        match purpose {
            PromptPurpose::File(flow) => self.file_prompt(flow, result),
            PromptPurpose::GotoLine => {
                if let PromptResult::Submit(input) = result {
                    self.goto_line(&input);
                }
            }
        }
    }

    /// Moves to the start of 1-based line `input`, clamped to the buffer.
    fn goto_line(&mut self, input: &str) {
        let s = input.trim();
        let last = self.editor.buf.rope.len_lines() - 1;
        let line = if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            None
        } else {
            // Only overflow can fail here; that is past the end too.
            Some(s.parse::<usize>().unwrap_or(usize::MAX))
        };
        match line {
            Some(n) => {
                let line = n.saturating_sub(1).min(last);
                self.editor.move_to(Pos { line, col: 0 }, false);
            }
            None => self.set_status("Not a line number"),
        }
    }

    /// Routes a key to the open find/replace session and applies its outcome.
    fn find_key(&mut self, key: &KeyEvent) {
        let bound = self.keymap.lookup(key);
        let Some(f) = &mut self.find else {
            return;
        };
        let out = f.handle_key(key, bound.is_some(), &mut self.editor);
        if out.edited {
            // Text left of a no-wrap anchor may have changed under the cursor.
            self.viewport.invalidate();
        }
        if out.close {
            self.last_search = f.query().map(str::to_string).or(self.last_search.take());
            self.find = None;
        }
        self.status_from(out.status);
        if let (true, Some(id)) = (out.forward, bound) {
            self.dispatch(id);
        }
    }

    fn status_from(&mut self, status: Option<String>) {
        if let Some(msg) = status {
            self.set_status(&msg);
        }
    }

    pub(crate) fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if self.palette.is_some() {
                    self.palette_key(&key);
                } else if self.find.is_some() {
                    self.find_key(&key);
                } else if self.prompt.is_some() {
                    self.prompt_key(&key);
                } else if let Some(id) = self.keymap.lookup(&key) {
                    self.dispatch(id);
                } else if let Some(c) = typed_char(&key) {
                    let mut b = [0; 4];
                    self.editor.type_text(c.encode_utf8(&mut b));
                }
            }
            Event::Resize(w, h) => {
                self.screen = (w, h);
                self.viewport.height = h.saturating_sub(2);
                self.viewport.width = w.saturating_sub(self.gutter());
                self.relayout();
            }
            Event::Paste(_) if self.palette.is_some() => {}
            Event::Paste(s) => match (&mut self.find, &mut self.prompt) {
                (Some(f), _) => {
                    let out = f.paste(&s, &mut self.editor);
                    self.status_from(out.status);
                }
                (None, Some((p, _))) => p.paste(&s),
                (None, None) => self.editor.insert_str(&normalise_paste(&s)),
            },
            Event::Mouse(m) => {
                self.handle_mouse_at(m, Instant::now());
                return;
            }
            _ => {}
        }
        // Each frame: an edit may have changed the line count's digits.
        self.fit_width();
        let e = &self.editor;
        layout::scroll_to_cursor(&e.buf.rope, &mut self.viewport, e.cursor, e.buf.version);
    }

    /// Mouse handling with an explicit clock, so double-click timing is testable.
    fn handle_mouse_at(&mut self, m: MouseEvent, now: Instant) {
        if self.prompt.is_some() || self.find.is_some() || self.palette.is_some() {
            return;
        }
        // The text area sits between the title row and the status row.
        let text_row =
            |m: &MouseEvent| (m.row >= 1 && m.row <= self.viewport.height).then(|| m.row - 1);
        // A click in the gutter lands at the start of its row.
        let x = m.column.saturating_sub(self.gutter());
        let (rope, vp) = (&self.editor.buf.rope, &self.viewport);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(y) = text_row(&m) else {
                    return;
                };
                let p = layout::screen_to_pos(rope, vp, x, y);
                let prev = self.last_click.take();
                let double = prev.is_some_and(|(t, x, y0)| {
                    (x, y0) == (m.column, m.row) && now.saturating_duration_since(t) < DOUBLE_CLICK
                });
                if double {
                    self.editor.select_word(p);
                } else {
                    self.editor.move_to(p, false);
                    self.last_click = Some((now, m.column, m.row));
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(y) = text_row(&m) else {
                    return;
                };
                let p = layout::screen_to_pos(rope, vp, x, y);
                self.editor.move_to(p, true);
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let rows = if m.kind == MouseEventKind::ScrollDown {
                    WHEEL_ROWS
                } else {
                    -WHEEL_ROWS
                };
                let version = self.editor.buf.version;
                layout::scroll_rows(rope, &mut self.viewport, rows, version);
                return;
            }
            _ => return,
        }
        let e = &self.editor;
        layout::scroll_to_cursor(&e.buf.rope, &mut self.viewport, e.cursor, e.buf.version);
    }

    pub(crate) fn from_buffer(buf: Buffer, path: Option<PathBuf>) -> App {
        App {
            editor: Editor::new(buf),
            path,
            viewport: Viewport::new(80, 22, true, TAB_WIDTH),
            screen: (80, 24),
            keymap: Keymap::defaults(),
            is_new: true,
            disk_mtime: None,
            read_only: false,
            prompt: None,
            find: None,
            palette: None,
            last_search: None,
            status: None,
            last_click: None,
            clip: Box::new(Osc52::default()),
            quit: false,
            line_numbers: false,
            recovery: Recovery::disabled(),
        }
    }
}

/// Bracketed paste can send `\r` for newlines; pasted text only.
fn normalise_paste(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Text typed by a key that no command claimed. Ctrl+Alt counts as text
/// because that is how AltGr arrives on Windows.
fn typed_char(key: &KeyEvent) -> Option<char> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char(c) if ctrl == alt && !c.is_control() => Some(c),
        _ => None,
    }
}

/// Right side of the title bar. The only place that builds it.
pub(crate) fn title_state(app: &App) -> String {
    let state = if app.editor.buf.dirty {
        "Modified".to_string()
    } else if app.is_new {
        "New File".to_string()
    } else {
        app.disk_mtime.map(fileio::format_mtime).unwrap_or_default()
    };
    if app.read_only {
        format!("{state} [Read-only]")
    } else {
        state
    }
}

/// Test helpers shared by the app and file-flow tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::App;
    use crate::buffer::{Buffer, LineEnding};
    use crate::cursor::Pos;

    pub(crate) fn existing(d: &tempfile::TempDir, text: &str) -> (PathBuf, App) {
        let p = d.path().join("a.txt");
        std::fs::write(&p, text).unwrap();
        let app = App::open(Some(p.clone()), None).unwrap();
        (p, app)
    }

    pub(crate) fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    pub(crate) fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_event(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    pub(crate) fn set_readonly(p: &std::path::Path, ro: bool) {
        let mut perm = std::fs::metadata(p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(ro);
        std::fs::set_permissions(p, perm).unwrap();
    }

    pub(crate) fn status(app: &App) -> &str {
        app.status.as_ref().map_or("", |(s, _)| s)
    }

    pub(crate) fn press(app: &mut App, c: char) {
        app.handle_event(key(KeyCode::Char(c), KeyModifiers::NONE));
    }

    pub(crate) fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    pub(crate) fn unnamed_with(text: &str) -> App {
        let mut app = App::from_buffer(Buffer::new_empty(LineEnding::Lf), None);
        typ(&mut app, text);
        app
    }

    pub(crate) fn submit_name(app: &mut App, p: &std::path::Path) {
        typ(app, &p.to_string_lossy());
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
    }

    pub(crate) fn ctrl(c: char) -> Event {
        key(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    use crate::buffer::LineEnding;
    use crate::cursor::Pos;

    use tempfile::tempdir;

    #[test]
    fn title_state_new_file() {
        let d = tempdir().unwrap();
        let app = App::open(Some(d.path().join("new.txt")), None).unwrap();
        assert_eq!(title_state(&app), "New File");
    }

    #[test]
    fn title_state_modified() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        assert_eq!(app.editor.buf.rope.to_string(), "xabc");
        assert_eq!(title_state(&app), "Modified");
    }

    #[test]
    fn title_state_clean_shows_mtime() {
        let d = tempdir().unwrap();
        let (p, app) = existing(&d, "abc\n");
        let want = fileio::format_mtime(fileio::mtime(&p).unwrap());
        assert_eq!(title_state(&app), want);
    }

    #[test]
    fn title_state_read_only_suffix() {
        let d = tempdir().unwrap();
        let p = d.path().join("ro.txt");
        std::fs::write(&p, "abc\n").unwrap();
        set_readonly(&p, true);
        let mut app = App::open(Some(p.clone()), None).unwrap();
        let clean = title_state(&app);
        typ(&mut app, "x");
        let modified = title_state(&app);
        set_readonly(&p, false);
        assert!(clean.ends_with(" [Read-only]"), "{clean}");
        assert!(clean.len() > " [Read-only]".len(), "{clean}");
        assert_eq!(modified, "Modified [Read-only]");
    }

    #[test]
    fn keys_reach_commands_and_text() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, "ab\ncd\n");
        let none = KeyModifiers::NONE;
        app.handle_event(key(KeyCode::Right, none));
        app.handle_event(key(KeyCode::Down, none));
        assert_eq!(app.editor.cursor, pos(1, 1));
        app.handle_event(key(KeyCode::Enter, none));
        app.handle_event(key(KeyCode::Backspace, none));
        app.handle_event(key(KeyCode::Backspace, none));
        app.handle_event(key(KeyCode::End, KeyModifiers::CONTROL));
        app.handle_event(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        app.handle_event(key(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        app.handle_event(key(KeyCode::Char('Z'), KeyModifiers::SHIFT));
        assert_eq!(app.editor.buf.rope.to_string(), "ab\nd@Z");
        app.handle_event(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert!(app.palette.is_some());
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(Event::Paste("1\r\n2".into()));
        assert_eq!(app.editor.buf.rope.to_string(), "ab\nd@Z1\n2");
    }

    #[test]
    fn resize_resnaps_top_to_a_row_boundary() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, &"a".repeat(30));
        app.handle_event(Event::Resize(10, 5));
        assert_eq!((app.viewport.width, app.viewport.height), (10, 3));
        app.dispatch(CommandId::FileEnd);
        app.handle_event(Event::Resize(10, 5));
        assert_eq!(app.viewport.top.start_col, 10);
        app.handle_event(Event::Resize(7, 5));
        assert_eq!(app.viewport.top.start_col % 7, 0, "{:?}", app.viewport.top);
        let e = &app.editor;
        let (_, y) = layout::cursor_screen(&e.buf.rope, &app.viewport, e.cursor).unwrap();
        assert!(y < 3);
        for (w, h) in [(0, 0), (1, 1), (2, 2), (1, 3)] {
            app.handle_event(Event::Resize(w, h));
            app.dispatch(CommandId::PageUp);
            app.dispatch(CommandId::Down);
        }
    }

    #[test]
    fn status_fades_after_four_seconds() {
        let mut app = App::from_buffer(Buffer::new_empty(LineEnding::Lf), None);
        app.dispatch(CommandId::Copy);
        let t0 = app.status.as_ref().unwrap().1;
        let at = |s| t0 + Duration::from_millis(s);
        assert!(!app.expire_status(at(3999)));
        assert_eq!(app.status_text(), Some("Nothing selected"));
        assert!(app.expire_status(at(4000)));
        assert_eq!(app.status_text(), None);
        assert!(!app.expire_status(at(5000)));
    }

    #[test]
    fn shift_right_selects_grapheme() {
        let mut app = unnamed_with("");
        app.editor.insert_str("e\u{301}x");
        app.editor.move_to(pos(0, 0), false);
        let sh = KeyModifiers::SHIFT;
        app.handle_event(key(KeyCode::Right, sh));
        assert_eq!(app.editor.selection(), Some(0..2));
        app.handle_event(key(KeyCode::Right, sh));
        assert_eq!(app.editor.selection(), Some(0..3));
        app.handle_event(key(KeyCode::Left, sh));
        assert_eq!(app.editor.selection(), Some(0..2));
    }

    #[test]
    fn left_collapses_selection_to_start() {
        let mut app = unnamed_with("abcdef");
        app.editor.move_to(pos(0, 2), false);
        let sh = KeyModifiers::SHIFT;
        app.handle_event(key(KeyCode::Right, sh));
        app.handle_event(key(KeyCode::Right, sh));
        app.handle_event(key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!((app.editor.cursor, app.editor.anchor), (pos(0, 2), None));
        app.handle_event(key(KeyCode::Right, sh));
        app.handle_event(key(KeyCode::Right, sh));
        app.handle_event(key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!((app.editor.cursor, app.editor.anchor), (pos(0, 4), None));
    }

    #[test]
    fn shift_down_keeps_column_and_select_all_via_key() {
        let mut app = unnamed_with("");
        app.editor.insert_str("abcdef\nab\nabcdef");
        app.editor.move_to(pos(0, 5), false);
        let sh = KeyModifiers::SHIFT;
        app.handle_event(key(KeyCode::Down, sh));
        app.handle_event(key(KeyCode::Down, sh));
        assert_eq!(app.editor.cursor, pos(2, 5));
        assert_eq!(app.editor.anchor, Some(pos(0, 5)));
        app.handle_event(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(app.editor.selection(), Some(0..16));
        app.handle_event(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.editor.anchor, None);
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn down(x: u16, y: u16) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), x, y)
    }

    fn drag(x: u16, y: u16) -> MouseEvent {
        mouse(MouseEventKind::Drag(MouseButton::Left), x, y)
    }

    fn doc(text: &str) -> App {
        let mut app = unnamed_with("");
        app.editor.insert_str(text);
        app.editor.move_to(pos(0, 0), false);
        app
    }

    #[test]
    fn drag_extends_selection() {
        let mut app = doc("hello world\nsecond");
        let t = Instant::now();
        app.handle_mouse_at(down(1, 1), t);
        assert_eq!((app.editor.cursor, app.editor.anchor), (pos(0, 1), None));
        app.handle_mouse_at(drag(4, 1), t);
        assert_eq!(app.editor.selection(), Some(1..4));
        app.handle_mouse_at(drag(2, 2), t);
        assert_eq!(app.editor.selection(), Some(1..14));
        // Title and status rows are not text.
        app.handle_mouse_at(down(0, 0), t);
        app.handle_mouse_at(down(0, 23), t);
        assert_eq!(app.editor.selection(), Some(1..14));
        // Other buttons and motion are ignored.
        let right = MouseEventKind::Down(MouseButton::Right);
        app.handle_mouse_at(mouse(right, 0, 1), t);
        app.handle_mouse_at(mouse(MouseEventKind::Moved, 0, 1), t);
        assert_eq!(app.editor.selection(), Some(1..14));
    }

    #[test]
    fn double_click_selects_word() {
        let mut app = doc("foo bar  baz\n\nx");
        let t = Instant::now();
        let ms = Duration::from_millis;
        app.handle_mouse_at(down(5, 1), t);
        assert_eq!(app.editor.selection(), None);
        app.handle_mouse_at(down(5, 1), t + ms(399));
        assert_eq!(app.editor.selection(), Some(4..7));
        // A third click starts over rather than re-selecting.
        app.handle_mouse_at(down(5, 1), t + ms(500));
        assert_eq!(app.editor.selection(), None);
        // Whitespace selects its run.
        app.handle_mouse_at(down(8, 1), t + ms(2000));
        app.handle_mouse_at(down(8, 1), t + ms(2100));
        assert_eq!(app.editor.selection(), Some(7..9));
        // Too slow, or a different cell: just a click.
        app.handle_mouse_at(down(1, 1), t + ms(3000));
        app.handle_mouse_at(down(1, 1), t + ms(3400));
        assert_eq!(app.editor.selection(), None);
        app.handle_mouse_at(down(1, 1), t + ms(3500));
        app.handle_mouse_at(down(2, 1), t + ms(3600));
        assert_eq!(app.editor.selection(), None);
        // Empty line: nothing to select.
        app.handle_mouse_at(down(0, 2), t + ms(4000));
        app.handle_mouse_at(down(0, 2), t + ms(4100));
        assert_eq!(app.editor.selection(), None);
    }

    #[test]
    fn wheel_scrolls_three_rows_without_moving_cursor() {
        let text = (0..30)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut app = doc(&text);
        let t = Instant::now();
        app.handle_mouse_at(mouse(MouseEventKind::ScrollDown, 0, 5), t);
        assert_eq!(app.viewport.top.line, 3);
        assert_eq!(app.editor.cursor, pos(0, 0));
        app.handle_event(Event::Mouse(mouse(MouseEventKind::ScrollDown, 0, 5)));
        assert_eq!(app.viewport.top.line, 6);
        app.handle_mouse_at(mouse(MouseEventKind::ScrollUp, 0, 5), t);
        assert_eq!(app.viewport.top.line, 3);
        for _ in 0..4 {
            app.handle_mouse_at(mouse(MouseEventKind::ScrollUp, 0, 5), t);
        }
        assert_eq!(app.viewport.top.line, 0);
        for _ in 0..20 {
            app.handle_mouse_at(mouse(MouseEventKind::ScrollDown, 0, 5), t);
        }
        assert_eq!(app.viewport.top.line, 29);
        assert_eq!(app.editor.cursor, pos(0, 0));
    }

    fn cursor_cell(app: &App) -> Option<(u16, u16)> {
        let e = &app.editor;
        layout::cursor_screen(&e.buf.rope, &app.viewport, e.cursor)
    }

    fn long_doc() -> App {
        let text = (0..30)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        doc(&text)
    }

    #[test]
    fn wheel_past_cursor_hides_cursor() {
        let mut app = long_doc();
        let t = Instant::now();
        assert_eq!(cursor_cell(&app), Some((0, 0)));
        app.handle_mouse_at(mouse(MouseEventKind::ScrollDown, 0, 5), t);
        assert_eq!(cursor_cell(&app), None);
    }

    #[test]
    fn typing_after_wheel_brings_cursor_back() {
        let mut app = long_doc();
        app.handle_mouse_at(mouse(MouseEventKind::ScrollDown, 0, 5), Instant::now());
        assert_eq!(cursor_cell(&app), None);
        typ(&mut app, "x");
        assert!(cursor_cell(&app).is_some());
    }

    use crate::clipboard::Clip;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct FakeClip(Rc<RefCell<Option<String>>>);

    impl Clip for FakeClip {
        fn get(&mut self) -> Option<String> {
            self.0.borrow().clone()
        }
        fn set(&mut self, s: &str) {
            *self.0.borrow_mut() = Some(s.to_string());
        }
    }

    fn clip_app(text: &str) -> (App, FakeClip) {
        let fake = FakeClip::default();
        let app = App::from_buffer(Buffer::from_str_detect(text), None)
            .with_clipboard(Box::new(fake.clone()));
        (app, fake)
    }

    #[test]
    fn copy_selection() {
        let (mut app, fake) = clip_app("hello world");
        app.editor.move_to(Pos { line: 0, col: 6 }, true);
        app.dispatch(CommandId::Copy);
        assert_eq!(fake.0.borrow().as_deref(), Some("hello "));
        assert_eq!(app.editor.buf.rope.to_string(), "hello world");
        assert_eq!(app.status_text(), None);
    }

    #[test]
    fn copy_without_selection_is_noop_with_status() {
        let (mut app, fake) = clip_app("abc");
        app.dispatch(CommandId::Copy);
        assert_eq!(status(&app), "Nothing selected");
        assert_eq!(*fake.0.borrow(), None);
        assert!(!app.editor.buf.dirty);
    }

    #[test]
    fn cut_line_without_selection() {
        let (mut app, fake) = clip_app("ab\ncd\n");
        app.dispatch(CommandId::Cut);
        assert_eq!(fake.0.borrow().as_deref(), Some("ab\n"));
        assert_eq!(app.editor.buf.rope.to_string(), "cd");
        assert_eq!(app.status_text(), None);
    }

    #[test]
    fn paste_inserts_text() {
        let (mut app, fake) = clip_app("ad");
        *fake.0.borrow_mut() = Some("b\r\nc".into());
        app.editor.move_to(Pos { line: 0, col: 1 }, false);
        app.handle_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(app.editor.buf.rope.to_string(), "ab\ncd");
    }

    #[test]
    fn paste_replaces_selection() {
        let (mut app, fake) = clip_app("hello world");
        *fake.0.borrow_mut() = Some("X".into());
        app.editor.move_to(Pos { line: 0, col: 5 }, true);
        app.dispatch(CommandId::Paste);
        assert_eq!(app.editor.buf.rope.to_string(), "X world");
    }

    #[test]
    fn paste_empty_clipboard_not_dirty() {
        let (mut app, fake) = clip_app("abc");
        app.dispatch(CommandId::Paste);
        *fake.0.borrow_mut() = Some(String::new());
        app.dispatch(CommandId::Paste);
        assert!(!app.editor.buf.dirty);
        assert_eq!(app.editor.buf.rope.to_string(), "abc");
    }

    #[test]
    fn copy_too_large_for_osc52_shows_status() {
        let mut app = App::from_buffer(Buffer::from_str_detect(&"a".repeat(80_000)), None);
        app.editor.select_all();
        app.dispatch(CommandId::Copy);
        assert_eq!(status(&app), crate::clipboard::TOO_LARGE);
    }

    #[test]
    fn ctrl_z_and_ctrl_y_undo_and_redo_typed_text() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(
            &d, "abc
",
        );
        typ(&mut app, "xy");
        let ctrl = KeyModifiers::CONTROL;
        app.handle_event(key(KeyCode::Char('z'), ctrl));
        assert_eq!(app.editor.buf.rope.to_string(), "abc");
        assert!(!app.editor.buf.dirty);
        app.handle_event(key(KeyCode::Char('y'), ctrl));
        assert_eq!(app.editor.buf.rope.to_string(), "xyabc");
        assert!(app.editor.buf.dirty);
    }

    #[test]
    fn paste_is_one_undo_step() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(
            &d, "abc
",
        );
        app.handle_event(Event::Paste(
            "one two
three"
                .into(),
        ));
        app.dispatch(CommandId::Undo);
        assert_eq!(app.editor.buf.rope.to_string(), "abc");
    }

    #[test]
    fn undo_restores_cursor() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(
            &d, "abc
",
        );
        typ(&mut app, "x");
        app.dispatch(CommandId::Undo);
        assert_eq!(app.editor.cursor, pos(0, 0));
    }

    #[test]
    fn paste_skips_autoindent() {
        let mut app = App::open(None, None).unwrap();
        typ(&mut app, "  - a");
        app.handle_event(Event::Paste("x\ny".into()));
        assert_eq!(app.editor.buf.rope.to_string(), "  - ax\ny");
    }

    #[test]
    fn bracketed_paste_cr_becomes_newline() {
        let mut app = App::open(None, None).unwrap();
        app.handle_event(Event::Paste("a\rb\r\nc".into()));
        assert_eq!(app.editor.buf.rope.to_string(), "a\nb\nc");
        app.dispatch(CommandId::Undo);
        assert_eq!(app.editor.buf.rope.to_string(), "");
    }

    #[test]
    fn enter_and_tab_keys_use_smart_rules() {
        let mut app = App::open(None, None).unwrap();
        app.handle_event(key(KeyCode::Tab, KeyModifiers::NONE));
        typ(&mut app, "- a");
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.editor.buf.rope.to_string(), "    - a\n    - ");
    }

    #[test]
    fn find_selects_then_other_command_keeps_match() {
        let mut app = App::from_buffer(Buffer::from_str_detect("one two three"), None);
        app.handle_event(ctrl('f'));
        assert!(app.find.is_some());
        typ(&mut app, "two");
        assert_eq!(app.editor.selection(), Some(4..7));
        // A bound command closes the prompt and then runs.
        app.handle_event(key(KeyCode::End, KeyModifiers::NONE));
        assert!(app.find.is_none());
        assert_eq!(app.editor.cursor, Pos { line: 0, col: 13 });
        // Esc closes and restores.
        app.handle_event(ctrl('f'));
        typ(&mut app, "one");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.find.is_none());
        assert_eq!(app.editor.cursor, Pos { line: 0, col: 13 });
    }

    #[test]
    fn replace_invalidates_viewport() {
        let mut app = App::from_buffer(Buffer::from_str_detect("ab ab"), None);
        app.handle_event(ctrl('r'));
        typ(&mut app, "ab");
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        typ(&mut app, "ZZZ");
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        // Pretend a no-wrap anchor was computed before the edit.
        let vp = &mut app.viewport;
        (vp.hscroll, vp.hscroll_col, vp.hscroll_line) = (7, 7, 0);
        app.find_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(app.editor.buf.rope.to_string(), "ZZZ ab");
        let vp = &app.viewport;
        assert_eq!((vp.hscroll, vp.hscroll_col, vp.hscroll_line), (0, 0, 0));
    }

    fn goto(app: &mut App, input: &str) {
        app.handle_event(ctrl('g'));
        typ(app, input);
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn goto_clamps() {
        let mut app = doc("a\nbb\nc");
        app.handle_event(ctrl('g'));
        match &app.prompt {
            Some((Prompt::Text { label, .. }, _)) => assert_eq!(label, "Line: "),
            _ => panic!("expected the line prompt"),
        }
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.editor.move_to(pos(1, 2), false);
        app.editor.move_to(pos(2, 1), true);
        goto(&mut app, "2");
        assert_eq!((app.editor.cursor, app.editor.anchor), (pos(1, 0), None));
        goto(&mut app, " 99 ");
        assert_eq!(app.editor.cursor, pos(2, 0));
        goto(&mut app, "99999999999999999999999");
        assert_eq!(app.editor.cursor, pos(2, 0));
        assert!(app.prompt.is_none());
    }

    #[test]
    fn goto_bad_input_message() {
        for input in ["", "  ", "abc", "-1", "2x", "1.5"] {
            let mut app = doc("a\nbb\nc");
            app.editor.move_to(pos(1, 1), false);
            goto(&mut app, input);
            assert_eq!(status(&app), "Not a line number", "{input:?}");
            assert_eq!(app.editor.cursor, pos(1, 1), "{input:?}");
        }
    }

    #[test]
    fn goto_zero_goes_to_first_line() {
        let mut app = doc("a\nbb\nc");
        app.editor.move_to(pos(2, 1), false);
        goto(&mut app, "0");
        assert_eq!(app.editor.cursor, pos(0, 0));
    }

    #[test]
    fn toggle_wrap_flips() {
        let mut app = doc(&format!("{}\nb", "a".repeat(30)));
        app.handle_event(Event::Resize(10, 4));
        app.dispatch(CommandId::End);
        app.handle_event(Event::Resize(10, 4));
        assert_eq!(app.viewport.top.start_col, 20);
        app.dispatch(CommandId::ToggleWrap);
        let vp = &app.viewport;
        assert!(!vp.wrap);
        assert_eq!(vp.top.start_col, 0);
        assert!(vp.hscroll > 0);
        assert_eq!(cursor_cell(&app), Some((9, 0)));
        app.dispatch(CommandId::ToggleWrap);
        let vp = &app.viewport;
        assert!(vp.wrap);
        assert_eq!((vp.hscroll, vp.hscroll_col), (0, 0));
        assert_eq!(vp.top.start_col % 10, 0, "{:?}", vp.top);
        assert!(cursor_cell(&app).is_some());
    }

    #[test]
    fn line_numbers_gutter_width() {
        let mut app = doc("1\n2\n3\n4\n5\n6\n7\n8\n9");
        app.handle_event(Event::Resize(20, 10));
        assert_eq!((app.gutter(), app.viewport.width), (0, 20));
        app.dispatch(CommandId::ToggleLineNumbers);
        assert_eq!((app.gutter(), app.viewport.width), (2, 18));
        // Line 10 needs another digit.
        app.handle_event(key(KeyCode::End, KeyModifiers::CONTROL));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!((app.gutter(), app.viewport.width), (3, 17));
        app.dispatch(CommandId::Undo);
        app.handle_event(key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!((app.gutter(), app.viewport.width), (2, 18));
        app.dispatch(CommandId::ToggleLineNumbers);
        assert_eq!((app.gutter(), app.viewport.width), (0, 20));
    }

    #[test]
    fn click_with_gutter_maps_correctly() {
        let mut text = "a".repeat(30);
        for i in 1..10 {
            text.push_str(&format!("\nl{i}"));
        }
        let mut app = doc(&text);
        app.handle_event(Event::Resize(20, 10));
        app.dispatch(CommandId::ToggleLineNumbers);
        assert_eq!((app.gutter(), app.viewport.width), (3, 17));
        let t = Instant::now();
        app.handle_mouse_at(down(5, 1), t);
        assert_eq!(app.editor.cursor, pos(0, 2));
        // The second row of the wrapped first line starts at col 17.
        app.handle_mouse_at(down(5, 2), t + Duration::from_secs(1));
        assert_eq!(app.editor.cursor, pos(0, 19));
        // Gutter clicks land on the start of the row.
        app.handle_mouse_at(down(1, 2), t + Duration::from_secs(2));
        assert_eq!(app.editor.cursor, pos(0, 17));
        app.handle_mouse_at(down(0, 3), t + Duration::from_secs(3));
        assert_eq!(app.editor.cursor, pos(1, 0));
        app.handle_mouse_at(drag(4, 3), t + Duration::from_secs(3));
        assert_eq!(app.editor.selection(), Some(31..32));
    }

    fn palette_type(app: &mut App, s: &str) {
        app.handle_event(ctrl('p'));
        typ(app, s);
    }

    #[test]
    fn palette_runs_save_as_prompt() {
        let mut app = doc("x");
        palette_type(&mut app, "sva");
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.palette.is_none());
        assert!(
            matches!(&app.prompt, Some((Prompt::Text { label, .. }, _)) if label == "Save as: ")
        );
    }

    #[test]
    fn palette_esc_closes() {
        let mut app = doc("x");
        palette_type(&mut app, "q");
        assert!(app.palette.is_some());
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.palette.is_none() && !app.quit);
        // While open, typing goes to the query, not the buffer.
        palette_type(&mut app, "abc");
        assert_eq!(app.editor.buf.rope.to_string(), "x");
    }

    /// Runs a find for `q`, then closes it with Esc so the query is remembered.
    fn search(app: &mut App, q: &str) {
        app.dispatch(CommandId::Find);
        typ(app, q);
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.editor.move_to(pos(0, 0), false);
    }

    #[test]
    fn find_next_repeats_last_query() {
        let mut app = doc("ab ab ab");
        search(&mut app, "ab");
        app.dispatch(CommandId::FindNext);
        assert_eq!(app.editor.selection(), Some(0..2));
        app.dispatch(CommandId::FindNext);
        assert_eq!(app.editor.selection(), Some(3..5));
        app.dispatch(CommandId::FindNext);
        app.dispatch(CommandId::FindNext);
        assert_eq!(app.editor.selection(), Some(0..2));
        app.dispatch(CommandId::FindPrev);
        assert_eq!(app.editor.selection(), Some(6..8));
        search(&mut app, "zz");
        app.dispatch(CommandId::FindNext);
        assert_eq!(status(&app), "Not found");
    }

    #[test]
    fn find_prev_wraps() {
        let mut app = doc("ab ab");
        search(&mut app, "ab");
        app.dispatch(CommandId::FindPrev);
        assert_eq!(app.editor.selection(), Some(3..5));
    }

    #[test]
    fn find_next_without_query_status() {
        let mut app = doc("ab");
        app.dispatch(CommandId::FindNext);
        assert_eq!(status(&app), "No previous search");
        app.dispatch(CommandId::FindPrev);
        assert_eq!(status(&app), "No previous search");
    }

    #[test]
    fn no_command_is_unavailable() {
        for c in crate::commands::COMMANDS {
            let mut app = doc("ab
cd");
            app.dispatch(c.id);
            assert_ne!(app.status_text(), Some("Not available yet"), "{:?}", c.id);
        }
    }
}
