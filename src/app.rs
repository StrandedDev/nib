use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::terminal;

use crate::buffer::{Buffer, LineEnding};
use crate::cli;
use crate::clipboard::{Clip, Osc52};
use crate::commands::CommandId;
use crate::cursor::{self, Pos};
use crate::edit::Editor;
use crate::fileio::{self, Loaded};
use crate::keymap::Keymap;
use crate::layout::{self, Viewport};
use crate::prompt::{Prompt, PromptResult};
use crate::render::{self, AppView};
use crate::terminal::Guard;

pub struct App {
    editor: Editor,
    path: Option<PathBuf>,
    viewport: Viewport,
    /// Terminal size (columns, rows).
    screen: (u16, u16),
    keymap: Keymap,
    /// Never saved: no file on disk yet.
    is_new: bool,
    disk_mtime: Option<SystemTime>,
    read_only: bool,
    prompt: Option<(Prompt, PromptPurpose)>,
    status: Option<(String, Instant)>,
    /// Time and cell of the last left-button press, for double-click.
    last_click: Option<(Instant, u16, u16)>,
    clip: Box<dyn Clip>,
    quit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptPurpose {
    SaveBeforeQuit,
    SaveName { then_quit: bool },
    ConfirmReplace { path: PathBuf, then_quit: bool },
}

const TAB_WIDTH: u8 = 4;
const STATUS_FADE: Duration = Duration::from_secs(4);
const WHEEL_ROWS: isize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const POLL: Duration = Duration::from_millis(250);
const SAVE_CHOICE: &str = "Save? (Y)es / (N)o / (C)ancel";

impl App {
    /// Loads `path` (or starts an unnamed buffer) and puts the cursor on
    /// 1-based `line`, clamped. Never writes to disk.
    pub fn open(path: Option<PathBuf>, line: Option<usize>) -> Result<App> {
        let Some(path) = path else {
            return Ok(App::from_buffer(Buffer::new_empty(LineEnding::Lf), None));
        };
        let name = file_name(&path);
        let loaded = fileio::load(&path, false).with_context(|| format!("cannot open {name}"))?;
        let (buf, is_new) = match loaded {
            Loaded::Text(b) => (b, false),
            Loaded::NotFound => (Buffer::new_empty(LineEnding::Lf), true),
            Loaded::Binary => bail!("{name} is not a UTF-8 text file"),
            Loaded::Directory => bail!("{name} is a directory"),
            Loaded::TooLarge(_) => bail!("{name} is larger than 50 MB"),
        };
        let mut app = App::from_buffer(buf, None);
        app.is_new = is_new;
        app.disk_mtime = fileio::mtime(&path);
        app.read_only = !is_new && !fileio::is_writable(&path);
        app.path = Some(path);
        if let Some(n) = line {
            let last = app.editor.buf.rope.len_lines() - 1;
            let line = n.saturating_sub(1).min(last);
            app.editor.move_to(Pos { line, col: 0 }, false);
        }
        Ok(app)
    }

    pub fn with_clipboard(mut self, clip: Box<dyn Clip>) -> App {
        self.clip = clip;
        self
    }

    /// The terminal loop. The only place that takes over the terminal; the
    /// guard restores it on every return.
    pub fn run(&mut self) -> Result<()> {
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
            redraw = self.expire_status(Instant::now()) || changed;
        }
        out.flush()?;
        Ok(())
    }

    fn view(&self) -> AppView<'_> {
        AppView {
            editor: &self.editor,
            viewport: &self.viewport,
            screen: self.screen,
            name: self
                .path
                .as_deref()
                .map_or("[No Name]".to_string(), file_name),
            state: title_state(self),
            status: self.status_text(),
            prompt: self.prompt.as_ref().map(|(p, _)| p),
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
            C::Tab => self.editor.insert_str(&" ".repeat(usize::from(TAB_WIDTH))),
            C::Quit => self.quit(),
            C::Copy => self.copy(),
            C::Cut => self.cut(),
            C::Paste => self.paste(),
            C::Save => {
                self.save(false);
            }
            _ => self.set_status("Not available yet"),
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

    /// Saves to the current path, or asks for a name first. Returns whether
    /// the buffer is now saved; `then_quit` quits after a save from the name prompt.
    fn save(&mut self, then_quit: bool) -> bool {
        match self.path.clone() {
            Some(path) => self.save_to(path),
            None => {
                let p = Prompt::text("File name: ");
                self.prompt = Some((p, PromptPurpose::SaveName { then_quit }));
                false
            }
        }
    }

    fn save_to(&mut self, path: PathBuf) -> bool {
        if self.read_only {
            self.set_status("Cannot save: permission denied \u{2014} use Save As");
            return false;
        }
        if let Err(e) = fileio::atomic_write(&path, &self.editor.buf.to_bytes()) {
            self.set_status(&format!("Save failed: {e}"));
            return false;
        }
        self.editor.buf.dirty = false;
        self.disk_mtime = fileio::mtime(&path);
        if self.is_new {
            self.read_only = !fileio::is_writable(&path);
            self.is_new = false;
        }
        self.path = Some(path);
        true
    }

    fn set_status(&mut self, msg: &str) {
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

    fn quit(&mut self) {
        if self.editor.buf.dirty {
            let p = Prompt::Choice {
                label: SAVE_CHOICE.to_string(),
                keys: &['y', 'n', 'c'],
            };
            self.prompt = Some((p, PromptPurpose::SaveBeforeQuit));
        } else {
            self.quit = true;
        }
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
        match (purpose, result) {
            (PromptPurpose::SaveName { then_quit }, PromptResult::Submit(name)) => {
                self.save_named(&name, then_quit);
            }
            (PromptPurpose::ConfirmReplace { path, then_quit }, PromptResult::Choice('y')) => {
                self.save_then(path, then_quit);
            }
            (PromptPurpose::ConfirmReplace { .. }, _) => self.set_status("Not saved"),
            (PromptPurpose::SaveBeforeQuit, PromptResult::Choice('y')) => {
                if self.save(true) {
                    self.quit = true;
                }
            }
            (PromptPurpose::SaveBeforeQuit, PromptResult::Choice('n')) => self.quit = true,
            _ => {}
        }
    }

    /// First save under a typed name. An existing target is only replaced
    /// after a yes.
    fn save_named(&mut self, name: &str, then_quit: bool) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        // Same rules as the command line: an existing name is kept as typed,
        // a new one gets the .txt rule. A `:N` suffix is dropped.
        let (path, _) = cli::resolve(name, |p| p.exists());
        let path = std::path::absolute(&path).unwrap_or(path);
        // `new_file_name` splits on both separators, so this does too.
        let no_name = name.ends_with(['/', '\\']) || Path::new(name).file_name().is_none();
        if no_name || path.is_dir() {
            self.set_status("Not a file name");
            return;
        }
        if path.exists() {
            let p = Prompt::Choice {
                label: format!("{} exists. Replace? (y/N)", file_name(&path)),
                keys: &['y', 'n'],
            };
            self.prompt = Some((p, PromptPurpose::ConfirmReplace { path, then_quit }));
        } else {
            self.save_then(path, then_quit);
        }
    }

    fn save_then(&mut self, path: PathBuf, then_quit: bool) {
        if self.save_to(path) && then_quit {
            self.quit = true;
        }
    }

    pub(crate) fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if self.prompt.is_some() {
                    self.prompt_key(&key);
                } else if let Some(id) = self.keymap.lookup(&key) {
                    self.dispatch(id);
                } else if let Some(c) = typed_char(&key) {
                    let mut b = [0; 4];
                    self.editor.insert_str(c.encode_utf8(&mut b));
                }
            }
            Event::Resize(w, h) => {
                self.screen = (w, h);
                let vp = &mut self.viewport;
                (vp.width, vp.height) = (w, h.saturating_sub(2));
                vp.invalidate();
                // Old row starts mean nothing at a new width; the line start
                // always is a row start, and the scroll below finds the cursor.
                vp.top = layout::Row {
                    line: vp.top.line,
                    start_col: 0,
                    end_col: 0,
                };
            }
            Event::Paste(s) => match &mut self.prompt {
                Some((p, _)) => p.paste(&s),
                None => self.editor.insert_str(&s),
            },
            Event::Mouse(m) => {
                self.handle_mouse_at(m, Instant::now());
                return;
            }
            _ => {}
        }
        let e = &self.editor;
        layout::scroll_to_cursor(&e.buf.rope, &mut self.viewport, e.cursor, e.buf.version);
    }

    /// Mouse handling with an explicit clock, so double-click timing is testable.
    fn handle_mouse_at(&mut self, m: MouseEvent, now: Instant) {
        if self.prompt.is_some() {
            return;
        }
        // The text area sits between the title row and the status row.
        let text_row =
            |m: &MouseEvent| (m.row >= 1 && m.row <= self.viewport.height).then(|| m.row - 1);
        let (rope, vp) = (&self.editor.buf.rope, &self.viewport);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(y) = text_row(&m) else {
                    return;
                };
                let p = layout::screen_to_pos(rope, vp, m.column, y);
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
                let p = layout::screen_to_pos(rope, vp, m.column, y);
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
            status: None,
            last_click: None,
            clip: Box::new(Osc52::default()),
            quit: false,
        }
    }
}

/// Last path component for display, or the whole path when it has none.
fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
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
fn title_state(app: &App) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    #[test]
    fn title_state_new_file() {
        let d = tempdir().unwrap();
        let app = App::open(Some(d.path().join("new.txt")), None).unwrap();
        assert_eq!(title_state(&app), "New File");
    }

    fn existing(d: &tempfile::TempDir, text: &str) -> (PathBuf, App) {
        let p = d.path().join("a.txt");
        std::fs::write(&p, text).unwrap();
        let app = App::open(Some(p.clone()), None).unwrap();
        (p, app)
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_event(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
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

    fn set_readonly(p: &std::path::Path, ro: bool) {
        let mut perm = std::fs::metadata(p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(ro);
        std::fs::set_permissions(p, perm).unwrap();
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
    fn quit_dirty_opens_choice_prompt() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        app.dispatch(CommandId::Quit);
        assert!(!app.quit);
        match &app.prompt {
            Some((Prompt::Choice { label, keys }, PromptPurpose::SaveBeforeQuit)) => {
                assert_eq!(label, "Save? (Y)es / (N)o / (C)ancel");
                assert_eq!(*keys, &['y', 'n', 'c']);
            }
            _ => panic!("expected the save choice"),
        }
    }

    #[test]
    fn save_writes_via_atomic_write() {
        let d = tempdir().unwrap();
        let p = d.path().join("new.txt");
        let mut app = App::open(Some(p.clone()), None).unwrap();
        typ(&mut app, "hi");
        app.dispatch(CommandId::Save);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi\n");
        assert!(!app.editor.buf.dirty);
        assert_eq!(app.disk_mtime, fileio::mtime(&p));
        assert_eq!(
            title_state(&app),
            fileio::format_mtime(app.disk_mtime.unwrap())
        );
    }

    fn status(app: &App) -> &str {
        app.status.as_ref().map_or("", |(s, _)| s)
    }

    #[test]
    fn save_read_only_fails_and_stays_dirty() {
        let d = tempdir().unwrap();
        let p = d.path().join("ro.txt");
        std::fs::write(&p, "abc\n").unwrap();
        set_readonly(&p, true);
        let mut app = App::open(Some(p.clone()), None).unwrap();
        typ(&mut app, "x");
        app.dispatch(CommandId::Save);
        set_readonly(&p, false);
        assert_eq!(
            status(&app),
            "Cannot save: permission denied \u{2014} use Save As"
        );
        assert!(app.editor.buf.dirty);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "abc\n");
    }

    #[test]
    fn save_failure_keeps_dirty() {
        let d = tempdir().unwrap();
        let p = d.path().join("no-such-dir").join("a.txt");
        let mut app = App::open(Some(p), None).unwrap();
        typ(&mut app, "x");
        app.dispatch(CommandId::Save);
        assert!(
            status(&app).starts_with("Save failed: "),
            "{}",
            status(&app)
        );
        assert!(app.editor.buf.dirty);
        assert_eq!(title_state(&app), "Modified");
    }

    #[test]
    fn save_unnamed_prompts_for_name() {
        let d = tempdir().unwrap();
        let mut app = App::from_buffer(Buffer::new_empty(LineEnding::Lf), None);
        typ(&mut app, "hi");
        app.dispatch(CommandId::Save);
        match &app.prompt {
            Some((Prompt::Text { label, input, .. }, _)) => {
                assert_eq!(label, "File name: ");
                assert!(input.is_empty());
            }
            _ => panic!("expected the name prompt"),
        }
        typ(&mut app, &d.path().join("notes").to_string_lossy());
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        let p = d.path().join("notes.txt");
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi\n");
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
        assert!(!app.editor.buf.dirty);
    }

    fn press(app: &mut App, c: char) {
        app.handle_event(key(KeyCode::Char(c), KeyModifiers::NONE));
    }

    #[test]
    fn quit_choice_no_cancel_and_clean() {
        let d = tempdir().unwrap();
        let (p, mut app) = existing(&d, "abc\n");
        app.dispatch(CommandId::Quit);
        assert!(app.quit, "clean buffer quits at once");

        let mut app = App::open(Some(p.clone()), None).unwrap();
        typ(&mut app, "x");
        app.dispatch(CommandId::Quit);
        press(&mut app, 'c');
        assert!(app.prompt.is_none() && !app.quit);
        app.dispatch(CommandId::Quit);
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.prompt.is_none() && !app.quit);
        app.dispatch(CommandId::Quit);
        press(&mut app, 'n');
        assert!(app.quit);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "abc\n");
    }

    #[test]
    fn quit_choice_yes_saves_then_quits() {
        let d = tempdir().unwrap();
        let (p, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        app.dispatch(CommandId::Quit);
        press(&mut app, 'Y');
        assert!(app.quit);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xabc\n");
    }

    #[test]
    fn quit_choice_yes_failed_save_stays_open() {
        let d = tempdir().unwrap();
        let p = d.path().join("no-such-dir").join("a.txt");
        let mut app = App::open(Some(p), None).unwrap();
        typ(&mut app, "x");
        app.dispatch(CommandId::Quit);
        press(&mut app, 'y');
        assert!(!app.quit);
        assert!(status(&app).starts_with("Save failed: "));
    }

    #[test]
    fn quit_choice_yes_unnamed_asks_name_then_quits() {
        let d = tempdir().unwrap();
        let mut app = App::from_buffer(Buffer::new_empty(LineEnding::Lf), None);
        typ(&mut app, "hi");
        app.dispatch(CommandId::Quit);
        press(&mut app, 'y');
        assert!(!app.quit);
        assert!(matches!(app.prompt, Some((Prompt::Text { .. }, _))));
        typ(&mut app, &d.path().join("n.md").to_string_lossy());
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.quit);
        assert_eq!(
            std::fs::read_to_string(d.path().join("n.md")).unwrap(),
            "hi\n"
        );
    }

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
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
        app.handle_event(key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(status(&app), "Not available yet");
        app.handle_event(Event::Paste("1\r\n2".into()));
        assert_eq!(app.editor.buf.rope.to_string(), "ab\nd@Z1\n2");
    }

    #[test]
    fn open_at_line_is_one_based_and_clamped() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        let at = |line| {
            App::open(Some(p.clone()), Some(line))
                .unwrap()
                .editor
                .cursor
        };
        assert_eq!(at(2), pos(1, 0));
        assert_eq!(at(0), pos(0, 0));
        assert_eq!(at(99), pos(2, 0));
    }

    fn open_err(p: PathBuf) -> String {
        match App::open(Some(p), None) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn open_rejects_directory_and_binary_untouched() {
        let d = tempdir().unwrap();
        let dir = d.path().join("sub");
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(open_err(dir), "sub is a directory");
        let bin = d.path().join("b.bin");
        std::fs::write(&bin, b"a\0b").unwrap();
        assert_eq!(open_err(bin.clone()), "b.bin is not a UTF-8 text file");
        assert_eq!(std::fs::read(&bin).unwrap(), b"a\0b");
        let big = d.path().join("big.txt");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(fileio::LARGE_FILE_BYTES + 1)
            .unwrap();
        assert_eq!(open_err(big), "big.txt is larger than 50 MB");
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
        app.dispatch(CommandId::Find);
        let t0 = app.status.as_ref().unwrap().1;
        let at = |s| t0 + Duration::from_millis(s);
        assert!(!app.expire_status(at(3999)));
        assert_eq!(app.status_text(), Some("Not available yet"));
        assert!(app.expire_status(at(4000)));
        assert_eq!(app.status_text(), None);
        assert!(!app.expire_status(at(5000)));
    }

    fn unnamed_with(text: &str) -> App {
        let mut app = App::from_buffer(Buffer::new_empty(LineEnding::Lf), None);
        typ(&mut app, text);
        app
    }

    fn submit_name(app: &mut App, p: &std::path::Path) {
        typ(app, &p.to_string_lossy());
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn name_prompt_existing_file_asks_before_replacing() {
        let d = tempdir().unwrap();
        let p = d.path().join("old.txt");
        std::fs::write(&p, "old").unwrap();
        let mut app = unnamed_with("hi");
        app.dispatch(CommandId::Save);
        submit_name(&mut app, &p);
        match &app.prompt {
            Some((Prompt::Choice { label, keys }, _)) => {
                assert_eq!(label, "old.txt exists. Replace? (y/N)");
                assert_eq!(*keys, &['y', 'n']);
            }
            _ => panic!("expected the replace choice"),
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        press(&mut app, 'y');
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi\n");
        assert!(!app.editor.buf.dirty);
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
    }

    #[test]
    fn name_prompt_decline_keeps_file_and_dirty() {
        let d = tempdir().unwrap();
        let p = d.path().join("old.txt");
        std::fs::write(&p, "old").unwrap();
        let decline: [Event; 3] = [
            key(KeyCode::Char('n'), KeyModifiers::NONE),
            key(KeyCode::Esc, KeyModifiers::NONE),
            key(KeyCode::Enter, KeyModifiers::NONE),
        ];
        for ev in decline {
            let mut app = unnamed_with("hi");
            app.dispatch(CommandId::Save);
            submit_name(&mut app, &p);
            app.handle_event(ev);
            assert!(app.prompt.is_none());
            assert_eq!(status(&app), "Not saved");
            assert!(app.editor.buf.dirty);
            assert!(app.path.is_none());
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        }
    }

    #[test]
    fn quit_yes_name_replace_yes_saves_and_quits() {
        let d = tempdir().unwrap();
        let p = d.path().join("old.txt");
        std::fs::write(&p, "old").unwrap();
        let mut app = unnamed_with("hi");
        app.dispatch(CommandId::Quit);
        press(&mut app, 'y');
        submit_name(&mut app, &p);
        assert!(!app.quit);
        press(&mut app, 'y');
        assert!(app.quit);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi\n");
    }

    #[test]
    fn name_prompt_existing_extensionless_not_renamed() {
        let d = tempdir().unwrap();
        let p = d.path().join("notes");
        std::fs::write(&p, "old").unwrap();
        let mut app = unnamed_with("hi");
        app.dispatch(CommandId::Save);
        submit_name(&mut app, &p);
        press(&mut app, 'y');
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi\n");
        assert!(!d.path().join("notes.txt").exists());
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
    }

    #[test]
    fn name_prompt_new_extensionless_gets_txt() {
        let d = tempdir().unwrap();
        let mut app = unnamed_with("hi");
        app.dispatch(CommandId::Save);
        submit_name(&mut app, &d.path().join("todo"));
        assert!(app.prompt.is_none());
        let p = d.path().join("todo.txt");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi\n");
        assert!(!d.path().join("todo").exists());
    }

    #[test]
    fn name_prompt_trailing_separator_rejected() {
        let d = tempdir().unwrap();
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let names = [
            format!("{}/", sub.display()),
            format!("{}\\", d.path().join("new").display()),
            sub.display().to_string(),
        ];
        for name in names {
            let mut app = unnamed_with("hi");
            app.dispatch(CommandId::Save);
            typ(&mut app, &name);
            app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
            assert!(app.prompt.is_none(), "{name}");
            assert_eq!(status(&app), "Not a file name", "{name}");
            assert!(app.editor.buf.dirty);
            assert!(app.path.is_none());
        }
        let left: Vec<_> = std::fs::read_dir(d.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "only sub/ remains");
        assert_eq!(std::fs::read_dir(&sub).unwrap().count(), 0);
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
}
