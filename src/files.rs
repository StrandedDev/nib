//! File flows over the app: loading at startup, save, the first-save name
//! prompt, the replace confirm and the save choice before quit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::app::{App, PromptPurpose};
use crate::buffer::{Buffer, LineEnding};
use crate::cli;
use crate::config::Config;
use crate::cursor::Pos;
use crate::edit::Editor;
use crate::fileio::{self, Loaded};
use crate::indent;
use crate::layout::Row;
use crate::prompt::{Prompt, PromptResult};
use crate::recovery::{Check, Recovery};

pub(crate) const SAVE_CHOICE: &str = "Save? (Y)es / (N)o / (C)ancel";

/// What an open prompt of a file flow is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileFlow {
    /// `Save? (Y)es / (N)o / (C)ancel` before leaving a dirty buffer.
    SaveChoice {
        then: Then,
    },
    SaveName {
        then: Then,
    },
    ConfirmReplace {
        path: PathBuf,
        then: Then,
    },
    /// `File changed on disk — overwrite? (y/N)`
    ExternalChange {
        path: PathBuf,
        then: Then,
    },
    OpenName,
    /// `already open in another nib …? (Y/n)`
    ReadOnly(PendingOpen),
    /// `Recover unsaved changes from …? (Y/n)`
    Recover(PendingOpen),
    /// `File is 2.1 GB — open anyway? (y/N)`
    TooLarge(PendingOpen),
}

/// An open waiting on a recovery or lock question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingOpen {
    path: PathBuf,
    line: Option<usize>,
    /// Nothing to go back to: declining leaves nib.
    startup: bool,
}

/// What follows a successful save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Then {
    Stay,
    Quit,
    Open,
}

impl App {
    /// Loads `path` (or starts an unnamed buffer) and puts the cursor on
    /// 1-based `line`, clamped. Never writes to disk.
    pub fn open(path: Option<PathBuf>, line: Option<usize>) -> Result<App> {
        App::open_with(path, line, Recovery::disabled())
    }

    /// `open` with crash recovery and the lock kept in `recovery`. A file
    /// with something to ask about opens once the prompt is answered.
    pub fn open_with(
        path: Option<PathBuf>,
        line: Option<usize>,
        recovery: Recovery,
    ) -> Result<App> {
        App::open_configured(path, line, recovery, Config::default())
    }

    /// `open_with` using the settings and keymap in `config`.
    pub fn open_configured(
        path: Option<PathBuf>,
        line: Option<usize>,
        recovery: Recovery,
        config: Config,
    ) -> Result<App> {
        let mut app = App::from_buffer(Buffer::new_empty(config.line_ending), None);
        app.new_line_ending = config.line_ending;
        app.keymap = config.keymap;
        app.viewport.wrap = config.wrap;
        app.line_numbers = config.line_numbers;
        app.tab_width = config.tab_width;
        app.set_indent(indent::detect_indent(&app.editor.buf.rope, ""));
        app.recovery = recovery;
        if let Some(path) = path {
            app.open_path(path, line, true, false)?;
        }
        Ok(app)
    }

    /// Reads `path` and opens it. A file nib cannot edit gets the message
    /// screen at startup (before any lock or recovery check) and a status
    /// message on Open; one over the size limit asks first.
    fn open_path(
        &mut self,
        path: PathBuf,
        line: Option<usize>,
        startup: bool,
        allow_large: bool,
    ) -> Result<()> {
        match read_for_edit(&path, allow_large, self.new_line_ending)? {
            Opened::Edit(buf, is_new) => self.open_checked(buf, is_new, path, line, startup),
            Opened::Refused(why) if startup => self.message = Some(why),
            Opened::Refused(why) => self.set_status(&why),
            Opened::TooLarge(bytes) => {
                let p = Prompt::Choice {
                    label: format!(
                        "File is {} \u{2014} open anyway? (y/N)",
                        fileio::format_size(bytes)
                    ),
                    keys: &['y', 'n'],
                    default: None,
                };
                let pending = PendingOpen {
                    path,
                    line,
                    startup,
                };
                self.ask(p, FileFlow::TooLarge(pending));
            }
        }
        Ok(())
    }

    /// Opens `path`, unless another nib has it or a crash left changes for
    /// it: then that is asked first.
    fn open_checked(
        &mut self,
        buf: Buffer,
        is_new: bool,
        path: PathBuf,
        line: Option<usize>,
        startup: bool,
    ) {
        let check = self.recovery.check(&path);
        let pending = PendingOpen {
            path,
            line,
            startup,
        };
        match check {
            Check::Clear => {
                self.replace_buffer(buf, is_new, pending.path, line, false);
                self.announce_open(startup);
            }
            Check::Owned(pid) => {
                let p = Prompt::Choice {
                    label: format!(
                        "already open in another nib (pid {pid}) \u{2014} open read-only? (Y/n)"
                    ),
                    keys: &['y', 'n'],
                    default: Some('y'),
                };
                self.ask(p, FileFlow::ReadOnly(pending));
            }
            Check::Stale(when) => {
                let p = Prompt::Choice {
                    label: format!(
                        "Recover unsaved changes from {}? (Y/n)",
                        fileio::format_mtime(when)
                    ),
                    keys: &['y', 'n'],
                    // Safe: the text loads as unsaved and the file is untouched.
                    default: Some('y'),
                };
                self.ask(p, FileFlow::Recover(pending));
            }
        }
    }

    /// Makes `buf` the buffer for `path`, resetting everything that belongs
    /// to the old one: undo history, cursor, indent, file state, scroll and
    /// any find session.
    /// A `shared` buffer is a read-only copy of a file another nib owns.
    fn replace_buffer(
        &mut self,
        buf: Buffer,
        is_new: bool,
        path: PathBuf,
        line: Option<usize>,
        shared: bool,
    ) {
        let shared = self.recovery.attach(&path, shared);
        self.editor = Editor::new(buf);
        self.set_indent(indent::detect_indent(
            &self.editor.buf.rope,
            &file_name(&path),
        ));
        self.is_new = is_new;
        self.disk_mtime = fileio::mtime(&path);
        self.read_only = shared || (!is_new && !fileio::is_writable(&path));
        self.path = Some(path);
        self.find = None;
        self.last_click = None;
        // `Buffer::version` restarts at 0, so cached layout cannot be trusted.
        self.viewport.invalidate();
        self.viewport.top = Row::default();
        if let Some(n) = line {
            let last = self.editor.buf.rope.len_lines() - 1;
            let line = n.saturating_sub(1).min(last);
            self.editor.move_to(Pos { line, col: 0 }, false);
        }
    }

    /// Says which file Open loaded. At startup the title bar is enough.
    fn announce_open(&mut self, startup: bool) {
        let Some(path) = &self.path else {
            return;
        };
        if !startup {
            let what = if self.is_new { "New file" } else { "Opened" };
            self.set_status(&format!("{what} {}", file_name(path)));
        }
    }

    /// Ctrl+O after the name prompt. A file that cannot be edited leaves the
    /// current buffer as it is.
    fn open_named(&mut self, input: &str) {
        let raw = input.trim();
        if raw.is_empty() {
            return;
        }
        let (path, line) = cli::resolve(raw, |p| p.exists());
        let path = std::path::absolute(&path).unwrap_or(path);
        if let Err(e) = self.open_path(path, line, false, false) {
            self.set_status(&format!("{e:#}"));
        }
    }

    /// Saves to the current path, or asks for a name first. Returns whether
    /// the buffer is now saved; `then` runs after a save from the name prompt.
    pub(crate) fn save(&mut self, then: Then) -> bool {
        match self.path.clone() {
            Some(path) => self.save_to(path, then),
            None => {
                let p = Prompt::text("File name: ");
                self.ask(p, FileFlow::SaveName { then });
                false
            }
        }
    }

    /// Save As: the name prompt, prefilled with the current path.
    pub(crate) fn save_as(&mut self) {
        let input = self
            .path
            .as_deref()
            .map_or(String::new(), |p| p.to_string_lossy().into_owned());
        let p = Prompt::Text {
            label: "Save as: ".to_string(),
            cursor: input.len(),
            input,
        };
        self.ask(p, FileFlow::SaveName { then: Then::Stay });
    }

    fn ask(&mut self, p: Prompt, flow: FileFlow) {
        self.prompt = Some((p, PromptPurpose::File(flow)));
    }

    /// Saves to `path` unless it is not writable or, for the buffer's own
    /// file, changed on disk since it was read: that is asked first, and
    /// `then` runs after a yes.
    fn save_to(&mut self, path: PathBuf, then: Then) -> bool {
        let own = self.path.as_ref() == Some(&path);
        // Save As to another file asks about that file, not this one.
        let read_only = if own {
            // Permissions may have been fixed, or a lock released, since the
            // file was opened. Another nib's copy stays read-only.
            if self.read_only && !self.recovery.is_shared() && fileio::is_writable(&path) {
                self.read_only = false;
            }
            self.read_only
        } else {
            path.exists() && !fileio::is_writable(&path)
        };
        if read_only {
            self.set_status("Cannot save: permission denied \u{2014} use Save As");
            return false;
        }
        // A file that is gone is simply written again.
        let on_disk = fileio::mtime(&path);
        if own && on_disk.is_some() && on_disk != self.disk_mtime {
            let p = Prompt::Choice {
                label: "File changed on disk \u{2014} overwrite? (y/N)".to_string(),
                keys: &['y', 'n'],
                default: None,
            };
            self.ask(p, FileFlow::ExternalChange { path, then });
            return false;
        }
        self.write_to(path)
    }

    /// Writes the buffer to `path` and makes it the buffer's file.
    fn write_to(&mut self, path: PathBuf) -> bool {
        if let Err(e) = fileio::atomic_write(&path, &self.editor.buf.to_bytes()) {
            self.set_status(&format!("Save failed: {e}"));
            return false;
        }
        self.editor.buf.dirty = false;
        self.editor.mark_saved();
        let shared = self.recovery.saved(&path);
        self.disk_mtime = fileio::mtime(&path);
        self.read_only = shared || !fileio::is_writable(&path);
        self.is_new = false;
        self.set_status(&format!("Saved {}", file_name(&path)));
        self.path = Some(path);
        true
    }

    pub(crate) fn quit(&mut self) {
        self.leave(Then::Quit);
    }

    /// Ctrl+O: the save choice first when dirty, then the name prompt.
    pub(crate) fn ask_open(&mut self) {
        self.leave(Then::Open);
    }

    /// Runs `then` once the buffer may be left, asking to save it first.
    fn leave(&mut self, then: Then) {
        if self.editor.buf.dirty {
            let p = Prompt::Choice {
                label: SAVE_CHOICE.to_string(),
                keys: &['y', 'n', 'c'],
                default: None,
            };
            self.ask(p, FileFlow::SaveChoice { then });
        } else {
            self.proceed(then);
        }
    }

    fn proceed(&mut self, then: Then) {
        match then {
            Then::Stay => {}
            Then::Quit => {
                self.recovery.close();
                self.quit = true;
            }
            Then::Open => self.ask(Prompt::text("Open: "), FileFlow::OpenName),
        }
    }

    /// Applies the answer to a file-flow prompt.
    pub(crate) fn file_prompt(&mut self, flow: FileFlow, result: PromptResult) {
        match (flow, result) {
            (FileFlow::SaveName { then }, PromptResult::Submit(name)) => {
                self.save_named(&name, then);
            }
            (FileFlow::ConfirmReplace { path, then }, PromptResult::Choice('y')) => {
                if !self.owned_elsewhere(&path) {
                    self.save_then(path, then);
                }
            }
            (FileFlow::ExternalChange { path, then }, PromptResult::Choice('y')) => {
                if !self.owned_elsewhere(&path) && self.write_to(path) {
                    self.proceed(then);
                }
            }
            (FileFlow::ConfirmReplace { .. } | FileFlow::ExternalChange { .. }, _) => {
                self.set_status("Not saved")
            }
            (FileFlow::SaveChoice { then }, PromptResult::Choice('y')) => {
                if self.save(then) {
                    self.proceed(then);
                }
            }
            (FileFlow::SaveChoice { then }, PromptResult::Choice('n')) => self.proceed(then),
            (FileFlow::OpenName, PromptResult::Submit(name)) => self.open_named(&name),
            (FileFlow::ReadOnly(o), PromptResult::Choice('y')) => {
                if let Some((buf, is_new)) = self.reread(&o.path) {
                    self.replace_buffer(buf, is_new, o.path, o.line, true);
                    self.announce_open(o.startup);
                }
            }
            (FileFlow::Recover(o), PromptResult::Choice('y')) => {
                let Some((disk, is_new)) = self.reread(&o.path) else {
                    return;
                };
                match self.recovery.payload(&o.path) {
                    Some(buf) => {
                        self.replace_buffer(buf, is_new, o.path.clone(), o.line, false);
                        self.editor.mark_unsaved();
                        self.recovery.adopt(&o.path);
                    }
                    None => self.replace_buffer(disk, is_new, o.path, o.line, false),
                }
                self.announce_open(o.startup);
            }
            (FileFlow::Recover(o), PromptResult::Choice('n')) => {
                match self.recovery.decline(&o.path) {
                    Ok(bak) => {
                        if let Some((buf, is_new)) = self.reread(&o.path) {
                            self.replace_buffer(buf, is_new, o.path, o.line, false);
                        }
                        self.set_status(&format!("Recovery kept at {}", bak.display()));
                    }
                    // Opening now would overwrite the recovery file.
                    Err(e) => {
                        self.set_status(&format!("Recovery file not moved: {e}"));
                        self.cancel_open(o.startup);
                    }
                }
            }
            (FileFlow::TooLarge(o), PromptResult::Choice('y')) => {
                if let Err(e) = self.open_path(o.path, o.line, o.startup, true) {
                    self.set_status(&format!("{e:#}"));
                    self.cancel_open(o.startup);
                }
            }
            // N to read-only or too large, and Esc to any, cancel the open.
            (FileFlow::ReadOnly(o) | FileFlow::Recover(o) | FileFlow::TooLarge(o), _) => {
                self.cancel_open(o.startup)
            }
            _ => {}
        }
    }

    /// Loads a file again after a prompt; it may have changed meanwhile.
    /// A large file only gets here after the size question was answered.
    fn reread(&mut self, path: &Path) -> Option<(Buffer, bool)> {
        match read_for_edit(path, true, self.new_line_ending) {
            Ok(Opened::Edit(buf, is_new)) => Some((buf, is_new)),
            Ok(Opened::Refused(why)) => {
                self.set_status(&why);
                None
            }
            Ok(Opened::TooLarge(_)) => unreachable!("read with allow_large"),
            Err(e) => {
                self.set_status(&format!("{e:#}"));
                None
            }
        }
    }

    /// A declined open keeps the current buffer; at startup there is none,
    /// so nib leaves.
    fn cancel_open(&mut self, startup: bool) {
        if startup {
            self.exit_note = self.status.take().map(|(s, _)| s);
            self.quit = true;
        }
    }

    /// First save under a typed name. An existing target is only replaced
    /// after a yes.
    fn save_named(&mut self, name: &str, then: Then) {
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
        if self.owned_elsewhere(&path) {
            return;
        }
        // Saving over the buffer's own file is just a save.
        if path.exists() && self.path.as_ref() != Some(&path) {
            let p = Prompt::Choice {
                label: format!("{} exists. Replace? (y/N)", file_name(&path)),
                keys: &['y', 'n'],
                default: None,
            };
            self.ask(p, FileFlow::ConfirmReplace { path, then });
        } else {
            self.save_then(path, then);
        }
    }

    /// Two nibs writing one file is what the lock is there to prevent. Asked
    /// again after every yes: another nib may have opened it meanwhile.
    fn owned_elsewhere(&mut self, path: &Path) -> bool {
        match self.recovery.check(path) {
            Check::Owned(pid) => {
                let name = file_name(path);
                self.set_status(&format!("{name} is open in another nib (pid {pid})"));
                true
            }
            _ => false,
        }
    }

    fn save_then(&mut self, path: PathBuf, then: Then) {
        if self.save_to(path, then) {
            self.proceed(then);
        }
    }
}

/// What reading a file for editing found.
enum Opened {
    /// The buffer and whether it is a new file.
    Edit(Buffer, bool),
    /// Not something nib can edit; the reason names the file.
    Refused(String),
    /// Over the size limit, in bytes: opened only after asking.
    TooLarge(u64),
}

fn read_for_edit(path: &Path, allow_large: bool, new_le: LineEnding) -> Result<Opened> {
    let name = file_name(path);
    let loaded = fileio::load(path, allow_large).with_context(|| format!("cannot open {name}"))?;
    Ok(match loaded {
        Loaded::Text(b) => Opened::Edit(b, false),
        Loaded::NotFound => Opened::Edit(Buffer::new_empty(new_le), true),
        Loaded::Binary => Opened::Refused(format!("{name} is not a UTF-8 text file")),
        Loaded::Directory => Opened::Refused(format!("{name} is a directory")),
        Loaded::NotRegular => Opened::Refused(format!("{name} is not a regular file")),
        Loaded::TooLarge(n) => Opened::TooLarge(n),
    })
}

/// Last path component for display, or the whole path when it has none.
pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{Event, KeyCode, KeyModifiers};
    use tempfile::tempdir;

    use crate::app::test_support::*;
    use crate::app::title_state;
    use crate::commands::CommandId;
    use crate::indent::Indent;

    #[test]
    fn quit_dirty_opens_choice_prompt() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        app.dispatch(CommandId::Quit);
        assert!(!app.quit);
        match &app.prompt {
            Some((
                Prompt::Choice { label, keys, .. },
                PromptPurpose::File(FileFlow::SaveChoice { then: Then::Quit }),
            )) => {
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

    fn crlf_config() -> Config {
        Config {
            line_ending: LineEnding::CrLf,
            ..Config::default()
        }
    }

    #[test]
    fn crlf_new_files() {
        let d = tempdir().unwrap();
        let p = d.path().join("new.txt");
        let mut app =
            App::open_configured(Some(p.clone()), None, Recovery::disabled(), crlf_config())
                .unwrap();
        typ(&mut app, "a");
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        typ(&mut app, "b");
        app.dispatch(CommandId::Save);
        assert_eq!(std::fs::read(&p).unwrap(), b"a\r\nb\r\n");

        // An existing file keeps its own line ending.
        let old = d.path().join("old.txt");
        std::fs::write(&old, "x\n").unwrap();
        let mut app =
            App::open_configured(Some(old.clone()), None, Recovery::disabled(), crlf_config())
                .unwrap();
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        app.dispatch(CommandId::Save);
        assert_eq!(std::fs::read(&old).unwrap(), b"\nx\n");

        let app = App::open_configured(None, None, Recovery::disabled(), crlf_config()).unwrap();
        assert_eq!(app.editor.buf.meta.line_ending, LineEnding::CrLf);
    }

    #[test]
    fn read_only_save_message() {
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

    /// Just over the 50 MB limit, and text, so it opens once allowed.
    fn big_text(d: &tempfile::TempDir) -> PathBuf {
        let p = d.path().join("big.txt");
        std::fs::write(&p, "abcdefghi\n".repeat(5_242_881)).unwrap();
        p
    }

    const BIG: &str = "File is 50.0 MB \u{2014} open anyway? (y/N)";

    #[test]
    fn too_large_prompts_at_startup() {
        let d = tempdir().unwrap();
        let p = big_text(&d);
        let mut app = App::open(Some(p.clone()), Some(2)).unwrap();
        assert_eq!(prompt_label(&app), BIG);
        assert!(app.path.is_none());
        press(&mut app, 'y');
        assert!(app.prompt.is_none() && !app.quit);
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
        assert_eq!(app.editor.cursor, pos(1, 0));
        assert_eq!(app.editor.buf.to_bytes().len(), 52_428_810);
        for c in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Enter] {
            let mut app = App::open(Some(p.clone()), None).unwrap();
            app.handle_event(key(c, KeyModifiers::NONE));
            assert!(app.quit, "{c:?}");
        }
    }

    #[test]
    fn too_large_prompts_on_open() {
        let d = tempdir().unwrap();
        let (a, mut app) = existing(&d, "abc\n");
        let big = big_text(&d);
        app.handle_event(ctrl('o'));
        submit_name(&mut app, &big);
        assert_eq!(prompt_label(&app), BIG);
        press(&mut app, 'n');
        assert!(app.prompt.is_none() && !app.quit);
        assert_eq!(app.path.as_deref(), Some(a.as_path()));
        assert_eq!(app.editor.buf.rope.to_string(), "abc");
        app.handle_event(ctrl('o'));
        submit_name(&mut app, &big);
        press(&mut app, 'y');
        assert_eq!(app.path.as_deref(), Some(big.as_path()));
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
            Some((Prompt::Choice { label, keys, .. }, _)) => {
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
    fn save_marks_undo_save_point() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        app.dispatch(CommandId::Save);
        app.dispatch(CommandId::Undo);
        assert!(app.editor.buf.dirty);
        app.dispatch(CommandId::Redo);
        assert!(!app.editor.buf.dirty);
    }

    #[test]
    fn config_tab_width_overrides_detection() {
        let d = tempdir().unwrap();
        let open = |name: &str, text: &str| {
            let p = d.path().join(name);
            std::fs::write(&p, text).unwrap();
            let cfg = Config {
                tab_width: Some(3),
                ..Config::default()
            };
            App::open_configured(Some(p), None, Recovery::disabled(), cfg).unwrap()
        };
        let app = open("a.yml", "a:\n  b:\n    c\n");
        assert_eq!(app.editor.indent, Indent::Spaces(3));
        assert_eq!(app.viewport.tab_width, 3);
        // A tab-indented file keeps real tabs, shown 3 columns wide.
        let app = open("Makefile", "all:\n\tcc\n");
        assert_eq!(app.editor.indent, Indent::Tabs);
        assert_eq!(app.viewport.tab_width, 3);
        let mut app = App::open_configured(
            None,
            None,
            Recovery::disabled(),
            Config {
                tab_width: Some(3),
                ..Config::default()
            },
        )
        .unwrap();
        app.handle_event(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.editor.buf.rope.to_string(), "   ");
    }

    #[test]
    fn new_unnamed_buffer_indent_is_four() {
        let mut app = App::open(None, None).unwrap();
        assert_eq!(app.editor.indent, Indent::Spaces(4));
        app.handle_event(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.editor.buf.rope.to_string(), "    ");
    }

    #[test]
    fn open_detects_indent_and_sets_tab_width() {
        let d = tempdir().unwrap();
        let (_, app) = existing(&d, "a:\n  b:\n    c\n");
        assert_eq!(app.editor.indent, Indent::Spaces(2));
        assert_eq!(app.viewport.tab_width, 2);
        let (_, app) = existing(&d, "a\n\tb\n\tc\n");
        assert_eq!(app.editor.indent, Indent::Tabs);
        assert_eq!(app.viewport.tab_width, 4);
    }

    #[test]
    fn quit_prompt_ctrl_n_does_not_quit() {
        let mut app = App::from_buffer(Buffer::from_str_detect("abc"), None);
        typ(&mut app, "x");
        app.dispatch(CommandId::Quit);
        app.handle_event(ctrl('n'));
        assert!(app.prompt.is_some() && !app.quit);
    }

    fn prompt_label(app: &App) -> &str {
        match &app.prompt {
            Some((Prompt::Text { label, .. }, _)) | Some((Prompt::Choice { label, .. }, _)) => {
                label
            }
            None => "",
        }
    }

    #[test]
    fn open_dirty_asks() {
        let d = tempdir().unwrap();
        let (p, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        app.handle_event(ctrl('o'));
        assert_eq!(prompt_label(&app), "Save? (Y)es / (N)o / (C)ancel");
        press(&mut app, 'c');
        assert!(app.prompt.is_none());
        app.handle_event(ctrl('o'));
        press(&mut app, 'n');
        assert_eq!(prompt_label(&app), "Open: ");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "abc\n");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(ctrl('o'));
        press(&mut app, 'y');
        assert_eq!(prompt_label(&app), "Open: ");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xabc\n");
        // A clean buffer goes straight to the name.
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(ctrl('o'));
        assert_eq!(prompt_label(&app), "Open: ");
        assert_eq!(app.editor.buf.rope.to_string(), "xabc");
    }

    fn open_name(app: &mut App, p: &std::path::Path) {
        app.handle_event(ctrl('o'));
        submit_name(app, p);
    }

    #[test]
    fn open_existing_replaces_buffer_and_resets_undo() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        app.dispatch(CommandId::Save);
        app.editor.move_to(pos(0, 2), true);
        let b = d.path().join("b.txt");
        std::fs::write(
            &b,
            "	one
	two\n",
        )
        .unwrap();
        // Pretend the old buffer was scrolled.
        let vp = &mut app.viewport;
        (vp.hscroll, vp.hscroll_col, vp.hscroll_line) = (7, 7, 0);
        open_name(&mut app, &b);
        assert!(app.prompt.is_none());
        assert_eq!(
            app.editor.buf.rope.to_string(),
            "	one
	two"
        );
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        assert_eq!((app.editor.cursor, app.editor.anchor), (pos(0, 0), None));
        assert_eq!(app.editor.indent, Indent::Tabs);
        assert!(!app.is_new && !app.read_only && !app.editor.buf.dirty);
        assert_eq!(app.disk_mtime, fileio::mtime(&b));
        let vp = &app.viewport;
        assert_eq!((vp.hscroll, vp.hscroll_col, vp.hscroll_line), (0, 0, 0));
        assert_eq!(vp.top, crate::layout::Row::default());
        app.dispatch(CommandId::Undo);
        assert_eq!(
            app.editor.buf.rope.to_string(),
            "	one
	two"
        );
        assert!(!app.editor.buf.dirty);
    }

    #[test]
    fn open_missing_makes_new_file_without_creating() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, "abc\n");
        open_name(&mut app, &d.path().join("fresh"));
        let p = d.path().join("fresh.txt");
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
        assert_eq!(app.editor.buf.rope.to_string(), "");
        assert_eq!(title_state(&app), "New File");
        assert!(!p.exists() && !d.path().join("fresh").exists());
        typ(&mut app, "hi");
        app.dispatch(CommandId::Save);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi\n");
    }

    #[test]
    fn open_binary_keeps_buffer_with_status() {
        let d = tempdir().unwrap();
        let (a, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let bin = d.path().join("b.bin");
        std::fs::write(&bin, b"a\0b").unwrap();
        let cases = [
            (bin.clone(), "b.bin is not a UTF-8 text file"),
            (sub, "sub is a directory"),
        ];
        for (p, msg) in cases {
            app.handle_event(ctrl('o'));
            press(&mut app, 'n');
            submit_name(&mut app, &p);
            assert_eq!(status(&app), msg);
            assert_eq!(app.editor.buf.rope.to_string(), "xabc");
            assert_eq!(app.path.as_deref(), Some(a.as_path()));
            assert!(app.editor.buf.dirty);
        }
        assert_eq!(std::fs::read(&bin).unwrap(), b"a\0b");
        // An empty name cancels without a word.
        app.status = None;
        app.handle_event(ctrl('o'));
        press(&mut app, 'n');
        typ(&mut app, "  ");
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.prompt.is_none() && app.status.is_none());
        assert_eq!(app.editor.buf.rope.to_string(), "xabc");
    }

    #[test]
    fn open_dirty_yes_save_fails_stays() {
        let d = tempdir().unwrap();
        let p = d.path().join("no-such-dir").join("a.txt");
        let mut app = App::open(Some(p.clone()), None).unwrap();
        typ(&mut app, "x");
        app.handle_event(ctrl('o'));
        press(&mut app, 'y');
        assert!(app.prompt.is_none());
        assert!(status(&app).starts_with("Save failed: "));
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
        assert_eq!(app.editor.buf.rope.to_string(), "x");
        // Unnamed: the name prompt comes first; declining the replace stays.
        let old = d.path().join("old.txt");
        std::fs::write(&old, "old").unwrap();
        let mut app = unnamed_with("hi");
        app.handle_event(ctrl('o'));
        press(&mut app, 'y');
        submit_name(&mut app, &old);
        press(&mut app, 'n');
        assert!(app.prompt.is_none());
        assert!(app.path.is_none() && app.editor.buf.dirty);
        // Accepting it saves, then asks what to open.
        app.handle_event(ctrl('o'));
        press(&mut app, 'y');
        submit_name(&mut app, &old);
        press(&mut app, 'y');
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "hi\n");
        match &app.prompt {
            Some((Prompt::Text { label, .. }, _)) => assert_eq!(label, "Open: "),
            _ => panic!("expected the open prompt"),
        }
    }

    #[test]
    fn open_with_line_suffix() {
        let d = tempdir().unwrap();
        let (_, mut app) = existing(&d, "abc\n");
        let b = d.path().join("b.md");
        std::fs::write(&b, "1\n2\n3\n").unwrap();
        app.handle_event(ctrl('o'));
        typ(&mut app, &format!("{}:2", b.display()));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        assert_eq!(app.editor.cursor, pos(1, 0));
    }

    /// Save As with the prefilled name replaced by `p`.
    fn save_as(app: &mut App, p: &std::path::Path) {
        app.dispatch(CommandId::SaveAs);
        while matches!(&app.prompt, Some((Prompt::Text { input, .. }, _)) if !input.is_empty()) {
            app.handle_event(key(KeyCode::Backspace, KeyModifiers::NONE));
        }
        submit_name(app, p);
    }

    #[test]
    fn save_as_changes_path() {
        let d = tempdir().unwrap();
        let (a, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        app.dispatch(CommandId::SaveAs);
        match &app.prompt {
            Some((
                Prompt::Text {
                    label,
                    input,
                    cursor,
                },
                _,
            )) => {
                assert_eq!(label, "Save as: ");
                assert_eq!(input, &a.to_string_lossy());
                assert_eq!(*cursor, input.len());
            }
            _ => panic!("expected the save-as prompt"),
        }
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        save_as(&mut app, &d.path().join("b"));
        let b = d.path().join("b.txt");
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "xabc\n");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "abc\n");
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        assert_eq!(app.disk_mtime, fileio::mtime(&b));
        assert!(!app.editor.buf.dirty && !app.is_new && !app.read_only);
        // History survives, and the save point moved with the save.
        app.dispatch(CommandId::Undo);
        assert_eq!(app.editor.buf.rope.to_string(), "abc");
        assert!(app.editor.buf.dirty);
        app.dispatch(CommandId::Redo);
        assert!(!app.editor.buf.dirty);
        // Unnamed: the prompt starts empty.
        let mut app = unnamed_with("hi");
        app.dispatch(CommandId::SaveAs);
        assert!(matches!(&app.prompt, Some((Prompt::Text { input, .. }, _)) if input.is_empty()));
    }

    #[test]
    fn save_as_same_path_no_confirm() {
        let d = tempdir().unwrap();
        let (a, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        app.dispatch(CommandId::SaveAs);
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "xabc\n");
        assert!(!app.editor.buf.dirty);
    }

    #[test]
    fn save_as_existing_asks_replace() {
        let d = tempdir().unwrap();
        let (a, mut app) = existing(&d, "abc\n");
        let b = d.path().join("b.txt");
        std::fs::write(&b, "old").unwrap();
        save_as(&mut app, &b);
        assert_eq!(prompt_label(&app), "b.txt exists. Replace? (y/N)");
        press(&mut app, 'n');
        assert_eq!(status(&app), "Not saved");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "old");
        assert_eq!(app.path.as_deref(), Some(a.as_path()));
        save_as(&mut app, &b);
        press(&mut app, 'y');
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "abc\n");
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        // An empty final component is not a name.
        save_as(&mut app, &d.path().join("sub/"));
        assert_eq!(status(&app), "Not a file name");
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
    }

    /// Another program rewrites `p`. The mtime is set by hand: two writes in
    /// one clock tick can share one.
    fn change_on_disk(p: &Path, text: &str) {
        std::fs::write(p, text).unwrap();
        let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    const CHANGED: &str = "File changed on disk \u{2014} overwrite? (y/N)";

    #[test]
    fn external_change_prompts() {
        let d = tempdir().unwrap();
        let (p, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        change_on_disk(&p, "theirs\n");
        app.dispatch(CommandId::Save);
        assert_eq!(prompt_label(&app), CHANGED);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "theirs\n");
        press(&mut app, 'y');
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xabc\n");
        assert!(!app.editor.buf.dirty);
        assert_eq!(app.disk_mtime, fileio::mtime(&p));
        // The refreshed mtime means the next save asks nothing.
        typ(&mut app, "y");
        app.dispatch(CommandId::Save);
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xyabc\n");
    }

    #[test]
    fn external_change_decline_keeps_dirty() {
        let d = tempdir().unwrap();
        for c in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Enter] {
            let (p, mut app) = existing(&d, "abc\n");
            typ(&mut app, "x");
            change_on_disk(&p, "theirs\n");
            app.dispatch(CommandId::Save);
            app.handle_event(key(c, KeyModifiers::NONE));
            assert!(app.prompt.is_none(), "{c:?}");
            assert_eq!(status(&app), "Not saved");
            assert!(app.editor.buf.dirty);
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "theirs\n");
        }
    }

    #[test]
    fn external_change_preserves_then_quit() {
        let d = tempdir().unwrap();
        let (p, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        change_on_disk(&p, "theirs\n");
        app.dispatch(CommandId::Quit);
        press(&mut app, 'y');
        assert_eq!(prompt_label(&app), CHANGED);
        press(&mut app, 'n');
        assert!(!app.quit && app.editor.buf.dirty);
        app.dispatch(CommandId::Quit);
        press(&mut app, 'y');
        press(&mut app, 'y');
        assert!(app.quit);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xabc\n");
        // Ctrl+O goes on to the name prompt the same way.
        let (p, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        change_on_disk(&p, "theirs\n");
        app.handle_event(ctrl('o'));
        press(&mut app, 'y');
        press(&mut app, 'y');
        assert_eq!(prompt_label(&app), "Open: ");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xabc\n");
    }

    #[test]
    fn save_as_skips_external_check() {
        let d = tempdir().unwrap();
        let (a, mut app) = existing(&d, "abc\n");
        typ(&mut app, "x");
        change_on_disk(&a, "theirs\n");
        let b = d.path().join("b.txt");
        save_as(&mut app, &b);
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "xabc\n");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "theirs\n");
        assert_eq!(app.disk_mtime, fileio::mtime(&b));
    }

    #[test]
    fn deleted_file_save_recreates() {
        let d = tempdir().unwrap();
        let (p, mut app) = existing(&d, "abc\n");
        std::fs::remove_file(&p).unwrap();
        typ(&mut app, "x");
        app.dispatch(CommandId::Save);
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xabc\n");
        assert!(!app.editor.buf.dirty);
        assert_eq!(app.disk_mtime, fileio::mtime(&p));
    }

    /// Another running nib (pid 4242) takes `p` while a prompt is open.
    fn taken_by_other_nib(d: &tempfile::TempDir, p: &Path) {
        let lock = crate::recovery::lock_path(&d.path().join("rec"), p);
        crate::recovery::write(&lock, 4242, Some(p), b"").unwrap();
    }

    fn open_with_recovery(d: &tempfile::TempDir, p: &Path) -> App {
        let rec = Recovery::new(d.path().join("rec"), |pid| pid == 4242);
        App::open_with(Some(p.to_path_buf()), None, rec).unwrap()
    }

    #[test]
    fn replace_confirm_rechecks_owner() {
        let d = tempdir().unwrap();
        let a = d.path().join("a.txt");
        std::fs::write(&a, "abc\n").unwrap();
        let mut app = open_with_recovery(&d, &a);
        typ(&mut app, "x");
        let b = d.path().join("b.txt");
        std::fs::write(&b, "theirs\n").unwrap();
        save_as(&mut app, &b);
        assert_eq!(prompt_label(&app), "b.txt exists. Replace? (y/N)");
        taken_by_other_nib(&d, &b);
        press(&mut app, 'y');
        assert_eq!(status(&app), "b.txt is open in another nib (pid 4242)");
        assert!(app.editor.buf.dirty);
        assert_eq!(app.path.as_deref(), Some(a.as_path()));
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "theirs\n");
    }

    #[test]
    fn external_change_confirm_rechecks_owner() {
        let d = tempdir().unwrap();
        let a = d.path().join("a.txt");
        std::fs::write(&a, "abc\n").unwrap();
        let mut app = open_with_recovery(&d, &a);
        typ(&mut app, "x");
        change_on_disk(&a, "theirs\n");
        app.dispatch(CommandId::Save);
        assert_eq!(prompt_label(&app), CHANGED);
        taken_by_other_nib(&d, &a);
        press(&mut app, 'y');
        assert_eq!(status(&app), "a.txt is open in another nib (pid 4242)");
        assert!(app.editor.buf.dirty);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "theirs\n");
    }

    #[test]
    fn write_error_keeps_dirty() {
        let d = tempdir().unwrap();
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let p = sub.join("a.txt");
        std::fs::write(&p, "abc\n").unwrap();
        let mut app = open_with_recovery(&d, &p);
        typ(&mut app, "x");
        assert!(!app.tick(std::time::Instant::now()));
        let rec = crate::recovery::rec_path(&d.path().join("rec"), Some(&p), 0);
        assert!(rec.exists());
        // The directory goes, so the temp file cannot be created.
        std::fs::remove_dir_all(&sub).unwrap();
        app.dispatch(CommandId::Save);
        assert!(
            status(&app).starts_with("Save failed: "),
            "{}",
            status(&app)
        );
        assert!(app.editor.buf.dirty);
        assert_eq!(title_state(&app), "Modified");
        assert!(rec.exists());
    }

    #[test]
    fn read_only_rechecked_on_save() {
        let d = tempdir().unwrap();
        let p = d.path().join("ro.txt");
        std::fs::write(&p, "abc\n").unwrap();
        set_readonly(&p, true);
        let mut app = App::open(Some(p.clone()), None).unwrap();
        set_readonly(&p, false);
        if !app.read_only {
            // root on unix can write anyway
            return;
        }
        typ(&mut app, "x");
        app.dispatch(CommandId::Save);
        assert_eq!(status(&app), "Saved ro.txt");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xabc\n");
        assert!(!app.read_only && !app.editor.buf.dirty);
        assert!(!title_state(&app).contains("[Read-only]"));
    }

    #[test]
    fn save_as_from_read_only_to_new_path() {
        let d = tempdir().unwrap();
        let ro = d.path().join("ro.txt");
        std::fs::write(&ro, "abc\n").unwrap();
        set_readonly(&ro, true);
        let mut app = App::open(Some(ro.clone()), None).unwrap();
        typ(&mut app, "x");
        // Another read-only file is refused even after a yes.
        let c = d.path().join("c.txt");
        std::fs::write(&c, "c").unwrap();
        set_readonly(&c, true);
        save_as(&mut app, &c);
        press(&mut app, 'y');
        let c_text = std::fs::read_to_string(&c).unwrap();
        set_readonly(&c, false);
        assert_eq!(c_text, "c");
        assert!(status(&app).starts_with("Cannot save: permission denied"));
        assert_eq!(app.path.as_deref(), Some(ro.as_path()));
        let b = d.path().join("b.txt");
        save_as(&mut app, &b);
        let ro_text = std::fs::read_to_string(&ro).unwrap();
        set_readonly(&ro, false);
        assert_eq!(ro_text, "abc\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "xabc\n");
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        assert!(!app.read_only && !app.editor.buf.dirty);
        assert!(!title_state(&app).contains("[Read-only]"));
    }
}
