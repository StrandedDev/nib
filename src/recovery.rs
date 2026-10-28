//! Crash recovery files and the one-nib-per-file lock.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::buffer::Buffer;
use crate::fileio;

/// FNV-1a, 64-bit. Names recovery files after the path they belong to.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn stem(path: &Path) -> String {
    format!("{:016x}", fnv1a64(path.as_os_str().as_encoded_bytes()))
}

/// The recovery file for `path`, or for this process's unnamed buffer.
pub fn rec_path(dir: &Path, path: Option<&Path>, pid: u32) -> PathBuf {
    match path {
        Some(p) => dir.join(format!("{}.rec", stem(p))),
        None => dir.join(format!("untitled-{pid}.rec")),
    }
}

pub fn lock_path(dir: &Path, path: &Path) -> PathBuf {
    dir.join(format!("{}.lock", stem(path)))
}

const MAGIC: &str = "nib-recovery 1";
const INTERVAL: Duration = Duration::from_secs(3);

/// The header of a recovery or lock file. `path` is for people only; the
/// file name is the key.
#[derive(Debug, PartialEq, Eq)]
pub struct Header {
    pub pid: u32,
    pub path: String,
}

/// Writes a recovery file (or, with an empty payload, a lock) atomically,
/// making its directory first.
pub fn write(file: &Path, pid: u32, path: Option<&Path>, payload: &[u8]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    // A newline in a file name would break the line-based header.
    let shown = path.map_or(String::new(), |p| {
        p.to_string_lossy().replace(['\n', '\r'], "?")
    });
    let mut bytes = format!("{MAGIC}\npid {pid}\npath {shown}\n\n").into_bytes();
    bytes.extend_from_slice(payload);
    fileio::atomic_write(file, &bytes)
}

/// The header and payload of `file`, or `None` when it is missing or not a
/// recovery file nib understands.
pub fn read(file: &Path) -> Option<(Header, Vec<u8>)> {
    let bytes = fs::read(file).ok()?;
    let mut rest = bytes.as_slice();
    let mut line = || {
        let r = rest;
        let end = r.iter().position(|b| *b == b'\n')?;
        rest = &r[end + 1..];
        Some(&r[..end])
    };
    let magic = line()?;
    let pid = line()?.strip_prefix(b"pid ")?;
    let path = line()?.strip_prefix(b"path ")?;
    let blank = line()?;
    if magic != MAGIC.as_bytes() || !blank.is_empty() {
        return None;
    }
    let pid = std::str::from_utf8(pid).ok()?.parse().ok()?;
    let path = String::from_utf8_lossy(path).into_owned();
    Some((Header { pid, path }, rest.to_vec()))
}

/// A recovery file nib can parse, with UTF-8 text.
fn readable(file: &Path) -> bool {
    read(file).is_some_and(|(_, p)| std::str::from_utf8(&p).is_ok())
}

/// Whether a process id belongs to a running process.
pub type Alive = fn(u32) -> bool;

/// Linux: `/proc/<pid>` exists. Any failure counts as not running.
#[cfg(target_os = "linux")]
pub fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Windows: `tasklist` lists the pid. Slow, but only runs when a file opens
/// and a recovery or lock file exists. Any failure counts as not running.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")))
}

/// Other unix systems are not supported: no owner is ever seen as running,
/// so a second nib offers recovery instead of read-only.
#[cfg(not(any(target_os = "linux", windows)))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}

/// What opening a file finds in the recovery directory.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    Clear,
    /// Another running nib has the file open.
    Owned(u32),
    /// A recovery file from a nib that is gone, written at this time.
    Stale(SystemTime),
}

/// Recovery and lock state of one nib. Without a directory every call does
/// nothing, which is what tests that do not ask for recovery get.
pub struct Recovery {
    dir: Option<PathBuf>,
    pub(crate) pid: u32,
    alive: Alive,
    /// The recovery file this nib wrote last.
    written: Option<PathBuf>,
    /// `Buffer::version` last written; `None` forces the next write.
    written_version: Option<u64>,
    last_write: Option<Instant>,
    /// The lock file this nib holds.
    lock: Option<PathBuf>,
    /// The buffer is a read-only copy of a file another nib owns: no lock
    /// and no recovery file for it.
    shared: bool,
    /// A failed write was reported; quiet until a write works again.
    failing: bool,
}

impl Recovery {
    pub fn new(dir: PathBuf, alive: Alive) -> Recovery {
        Recovery {
            dir: Some(dir),
            alive,
            ..Recovery::disabled()
        }
    }

    /// The real directory: `%LOCALAPPDATA%\nib\recovery` on Windows,
    /// `~/.local/share/nib/recovery` on Linux. Only startup uses it.
    pub fn system() -> Recovery {
        match dirs::data_local_dir() {
            Some(d) => Recovery::new(d.join("nib").join("recovery"), pid_alive),
            None => Recovery::disabled(),
        }
    }

    pub fn disabled() -> Recovery {
        Recovery {
            dir: None,
            pid: std::process::id(),
            alive: |_| false,
            written: None,
            written_version: None,
            last_write: None,
            lock: None,
            shared: false,
            failing: false,
        }
    }

    /// Who else has `path` open, or what a crash left behind for it.
    pub fn check(&self, path: &Path) -> Check {
        let Some(dir) = &self.dir else {
            return Check::Clear;
        };
        let file = rec_path(dir, Some(path), self.pid);
        // Our own file for the buffer being replaced is about to go.
        if self.written.as_ref() == Some(&file) {
            return Check::Clear;
        }
        for f in [lock_path(dir, path), file.clone()] {
            if let Some((h, _)) = read(&f) {
                if h.pid != self.pid && (self.alive)(h.pid) {
                    return Check::Owned(h.pid);
                }
            }
        }
        if !readable(&file) {
            return Check::Clear;
        }
        // Without a time it is still offered: opening would overwrite it.
        Check::Stale(fileio::mtime(&file).unwrap_or(SystemTime::UNIX_EPOCH))
    }

    /// Called every loop turn: writes the recovery file for a dirty buffer
    /// that changed since the last write, and drops it once the buffer is
    /// clean again (undo back to the save point).
    pub fn tick(&mut self, now: Instant, buf: &Buffer, path: Option<&Path>) -> Option<String> {
        if !buf.dirty {
            self.discard();
            return None;
        }
        let soon = self
            .last_write
            .is_some_and(|t| now.saturating_duration_since(t) < INTERVAL);
        if self.written_version == Some(buf.version) || soon {
            return None;
        }
        self.last_write = Some(now);
        match self.flush(buf, path)? {
            Ok(()) => {
                self.failing = false;
                None
            }
            Err(e) if !self.failing => {
                self.failing = true;
                Some(format!("Recovery file not written: {e}"))
            }
            Err(_) => None,
        }
    }

    /// Writes the recovery file for a dirty buffer now. `None` when there is
    /// nothing to write.
    pub fn flush(&mut self, buf: &Buffer, path: Option<&Path>) -> Option<io::Result<()>> {
        let dir = self.dir.as_ref().filter(|_| buf.dirty && !self.shared)?;
        let file = rec_path(dir, path, self.pid);
        if self.written.as_ref() != Some(&file) && file.exists() && !readable(&file) {
            let msg = format!("{} is not a nib recovery file", file.display());
            return Some(Err(io::Error::other(msg)));
        }
        let r = write(&file, self.pid, path, &buf.to_bytes());
        if r.is_ok() {
            self.written = Some(file);
            self.written_version = Some(buf.version);
        }
        Some(r)
    }

    /// The buffer now belongs to `path`: what was kept for the old one goes,
    /// and the lock moves to `path` unless the buffer is a `shared` copy.
    pub fn attach(&mut self, path: &Path, shared: bool) {
        self.close();
        self.shared = shared;
        if let (Some(dir), false) = (&self.dir, shared) {
            let lock = lock_path(dir, path);
            // Best effort: without it a second nib only misses the warning.
            if write(&lock, self.pid, Some(path), b"").is_ok() {
                self.lock = Some(lock);
            }
        }
    }

    /// Clean exit or leaving the buffer: no recovery file, no lock.
    pub fn close(&mut self) {
        self.discard();
        if let Some(l) = self.lock.take() {
            let _ = fs::remove_file(l);
        }
    }

    /// Takes over the recovery file for `path` after its text was
    /// recovered: it stays until the next successful save.
    pub fn adopt(&mut self, path: &Path) {
        if let Some(dir) = &self.dir {
            self.written = Some(rec_path(dir, Some(path), self.pid));
        }
    }

    /// After a successful save to `path`: the file on disk is the safe copy
    /// now. Save As moves the lock to the new file, which is ours even when
    /// the old one was a read-only copy.
    pub fn saved(&mut self, path: &Path) {
        self.discard();
        let lock = self.dir.as_ref().map(|d| lock_path(d, path));
        if self.shared || self.lock != lock {
            self.attach(path, false);
        }
    }

    /// Removes the recovery file this nib wrote, if any.
    fn discard(&mut self) {
        if let Some(f) = self.written.take() {
            let _ = fs::remove_file(f);
        }
        self.written_version = None;
    }

    /// Moves the recovery file for `path` aside to `<hash>.rec.bak`,
    /// replacing an older one, so a mistaken no can still be undone by hand.
    pub fn decline(&self, path: &Path) -> io::Result<PathBuf> {
        let Some(dir) = &self.dir else {
            return Err(io::Error::other("no recovery directory"));
        };
        let file = rec_path(dir, Some(path), self.pid);
        let bak = file.with_extension("rec.bak");
        fs::rename(&file, &bak)?;
        Ok(bak)
    }

    /// The text saved in the recovery file for `path`, if nib can read it.
    pub fn payload(&self, path: &Path) -> Option<Buffer> {
        let file = rec_path(self.dir.as_ref()?, Some(path), self.pid);
        let (_, payload) = read(&file)?;
        let text = String::from_utf8(payload).ok()?;
        Some(Buffer::from_str_detect(&text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;
    use crate::app::{App, title_state};
    use crate::commands::CommandId;
    use crate::prompt::Prompt;
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::time::Duration;
    use tempfile::{TempDir, tempdir};

    fn dead(_: u32) -> bool {
        false
    }

    #[test]
    fn unparseable_recovery_left_alone() {
        let d = tempdir().unwrap();
        let rec = Recovery::new(d.path().to_path_buf(), dead);
        let p = d.path().join("a.txt");
        let file = rec_path(d.path(), Some(&p), 1);
        let bad: [&[u8]; 4] = [
            b"garbage",
            b"nib-recovery 2\npid 5\npath x\n\nhi",
            b"nib-recovery 1\npid five\npath x\n\nhi",
            b"nib-recovery 1\npid 5\npath x\n\n\xff\xfe",
        ];
        for bytes in bad {
            std::fs::write(&file, bytes).unwrap();
            assert_eq!(rec.check(&p), Check::Clear);
            assert_eq!(std::fs::read(&file).unwrap(), bytes);
        }
        // Editing the file does not write over it either.
        std::fs::write(
            &p, "abc
",
        )
        .unwrap();
        let mut app = App::open_with(Some(p.clone()), None, rec).unwrap();
        typ(&mut app, "x");
        assert!(app.tick(Instant::now()));
        assert!(
            status(&app).contains("not a nib recovery file"),
            "{}",
            status(&app)
        );
        app.on_fatal_error();
        assert_eq!(std::fs::read(&file).unwrap(), bad[3]);
    }

    fn label(app: &App) -> &str {
        match &app.prompt {
            Some((Prompt::Text { label, .. }, _)) | Some((Prompt::Choice { label, .. }, _)) => {
                label
            }
            None => "",
        }
    }

    /// A file `a.txt` holding `disk` and a recovery file for it from `pid`.
    fn crashed(d: &TempDir, disk: &str, pid: u32, payload: &[u8]) -> (PathBuf, PathBuf) {
        let p = d.path().join("a.txt");
        std::fs::write(&p, disk).unwrap();
        let file = rec_path(&d.path().join("rec"), Some(&p), pid);
        write(&file, pid, Some(&p), payload).unwrap();
        (p, file)
    }

    fn open(d: &TempDir, p: &Path, alive: Alive) -> App {
        let rec = Recovery::new(d.path().join("rec"), alive);
        App::open_with(Some(p.to_path_buf()), None, rec).unwrap()
    }

    #[test]
    fn stale_pid_offers_recovery() {
        let d = tempdir().unwrap();
        let (p, file) = crashed(&d, "disk\n", 999, b"work\n");
        let when = fileio::format_mtime(fileio::mtime(&file).unwrap());
        let app = open(&d, &p, dead);
        assert_eq!(
            label(&app),
            format!("Recover unsaved changes from {when}? (Y/n)")
        );
        // Our own pid is no owner either.
        let mut rec = Recovery::new(d.path().join("rec"), |_| true);
        rec.pid = 999;
        let app = App::open_with(Some(p), None, rec).unwrap();
        assert!(label(&app).starts_with("Recover unsaved changes from "));
    }

    #[test]
    fn live_pid_offers_read_only() {
        let d = tempdir().unwrap();
        let me = std::process::id();
        assert!(pid_alive(me));
        assert!(!pid_alive(u32::MAX - 1));
        let (p, file) = crashed(&d, "disk\n", me, b"work\n");
        let want = format!("already open in another nib (pid {me}) \u{2014} open read-only? (Y/n)");
        let other = || {
            let mut rec = Recovery::new(d.path().join("rec"), pid_alive);
            rec.pid = u32::MAX;
            App::open_with(Some(p.clone()), None, rec).unwrap()
        };
        // A dirty owner holds a recovery file; a clean one only the lock.
        assert_eq!(label(&other()), want);
        std::fs::remove_file(&file).unwrap();
        write(&lock_path(&d.path().join("rec"), &p), me, Some(&p), b"").unwrap();
        assert_eq!(label(&other()), want);
    }

    fn alive(_: u32) -> bool {
        true
    }

    /// `a.txt` holding `disk`, locked by a live nib with pid 4242.
    fn locked(d: &TempDir, disk: &str) -> PathBuf {
        let p = d.path().join("a.txt");
        std::fs::write(&p, disk).unwrap();
        write(&lock_path(&d.path().join("rec"), &p), 4242, Some(&p), b"").unwrap();
        p
    }

    #[test]
    fn read_only_choice_enter_means_yes() {
        let d = tempdir().unwrap();
        let p = locked(&d, "disk\n");
        let mut app = open(&d, &p, alive);
        assert!(app.path.is_none());
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.prompt.is_none());
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
        assert_eq!(app.editor.buf.rope.to_string(), "disk");
        assert!(app.read_only && !app.quit);
        assert!(title_state(&app).ends_with(" [Read-only]"));
        typ(&mut app, "x");
        app.dispatch(CommandId::Save);
        assert_eq!(
            status(&app),
            "Cannot save: permission denied \u{2014} use Save As"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "disk\n");
        // N or Esc at startup leaves nib.
        for ev in [
            key(KeyCode::Char('n'), KeyModifiers::NONE),
            key(KeyCode::Esc, KeyModifiers::NONE),
        ] {
            let mut app = open(&d, &p, alive);
            app.handle_event(ev);
            assert!(app.quit && app.path.is_none());
        }
        // On Ctrl+O a no keeps the current buffer.
        let b = d.path().join("b.txt");
        std::fs::write(&b, "b\n").unwrap();
        let mut app = open(&d, &b, alive);
        ctrl_open(&mut app, &p);
        press(&mut app, 'n');
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        assert!(!app.quit && !app.read_only);
    }

    #[test]
    fn accept_recovery_loads_dirty_payload_disk_untouched() {
        let d = tempdir().unwrap();
        let (p, file) = crashed(&d, "disk\n", 999, b"work\r\nmore\r\n");
        let mut app = open(&d, &p, dead);
        press(&mut app, 'y');
        assert!(app.prompt.is_none());
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
        assert_eq!(app.editor.buf.rope.to_string(), "work\nmore");
        assert_eq!(app.editor.buf.to_bytes(), b"work\r\nmore\r\n");
        assert!(app.editor.buf.dirty);
        assert_eq!(title_state(&app), "Modified");
        assert_eq!(app.disk_mtime, fileio::mtime(&p));
        // No undo state matches the disk any more.
        typ(&mut app, "x");
        app.dispatch(CommandId::Undo);
        assert!(app.editor.buf.dirty);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "disk\n");
        assert!(file.exists());
        // Kept until the next successful save, even one before any tick.
        app.dispatch(CommandId::Save);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "work\r\nmore\r\n");
        assert!(!file.exists());
    }

    #[test]
    fn decline_recovery_keeps_bak() {
        let d = tempdir().unwrap();
        let (p, file) = crashed(&d, "disk\n", 999, b"work\n");
        let rec_bytes = std::fs::read(&file).unwrap();
        let bak = file.with_extension("rec.bak");
        std::fs::write(&bak, "older").unwrap();
        // Enter and Esc cancel: nothing is touched and nib leaves.
        for ev in [
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Esc, KeyModifiers::NONE),
        ] {
            let mut app = open(&d, &p, dead);
            app.handle_event(ev);
            assert!(app.quit && app.path.is_none());
            assert_eq!(std::fs::read(&file).unwrap(), rec_bytes);
        }
        let mut app = open(&d, &p, dead);
        press(&mut app, 'n');
        assert_eq!(app.path.as_deref(), Some(p.as_path()));
        assert_eq!(app.editor.buf.rope.to_string(), "disk");
        assert!(!app.editor.buf.dirty && !app.quit);
        assert!(!file.exists());
        assert_eq!(std::fs::read(&bak).unwrap(), rec_bytes);
        assert_eq!(status(&app), format!("Recovery kept at {}", bak.display()));
    }

    /// `a.txt` holding `abc`, its recovery file path, and nib open on it.
    fn clean(d: &TempDir) -> (PathBuf, PathBuf, App) {
        let p = d.path().join("a.txt");
        std::fs::write(&p, "abc\n").unwrap();
        let file = rec_path(&d.path().join("rec"), Some(&p), 1);
        let app = open(d, &p, dead);
        (p, file, app)
    }

    fn secs(t: Instant, s: u64) -> Instant {
        t + Duration::from_secs(s)
    }

    #[test]
    fn recovery_written_only_when_version_changes() {
        let d = tempdir().unwrap();
        let (_, file, mut app) = clean(&d);
        let t = Instant::now();
        app.tick(t);
        assert!(!file.exists(), "clean: nothing to recover");
        typ(&mut app, "x");
        app.tick(t);
        let (h, payload) = read(&file).unwrap();
        assert_eq!(
            (h.pid, payload.as_slice()),
            (std::process::id(), &b"xabc\n"[..])
        );
        std::fs::remove_file(&file).unwrap();
        app.tick(secs(t, 10));
        assert!(!file.exists(), "same version: not written again");
        typ(&mut app, "y");
        app.tick(secs(t, 10));
        assert_eq!(read(&file).unwrap().1, b"xyabc\n");
    }

    #[test]
    fn recovery_every_three_seconds() {
        let d = tempdir().unwrap();
        let (_, file, mut app) = clean(&d);
        let t = Instant::now();
        typ(&mut app, "x");
        app.tick(t);
        typ(&mut app, "y");
        app.tick(secs(t, 1));
        app.tick(t + Duration::from_millis(2999));
        assert_eq!(read(&file).unwrap().1, b"xabc\n");
        app.tick(secs(t, 3));
        assert_eq!(read(&file).unwrap().1, b"xyabc\n");
    }

    #[test]
    fn failed_recovery_write_status_once() {
        let d = tempdir().unwrap();
        let (_, file, mut app) = clean(&d);
        // A file where the directory should be: every write fails.
        std::fs::remove_dir_all(d.path().join("rec")).unwrap();
        std::fs::write(d.path().join("rec"), "").unwrap();
        let t = Instant::now();
        typ(&mut app, "x");
        assert!(app.tick(t));
        assert!(
            status(&app).starts_with("Recovery file not written: "),
            "{}",
            status(&app)
        );
        app.status = None;
        typ(&mut app, "y");
        assert!(!app.tick(secs(t, 3)));
        assert!(app.status.is_none());
        assert_eq!(app.editor.buf.rope.to_string(), "xyabc");
        // Once writes work again, a new failure is reported again.
        std::fs::remove_file(d.path().join("rec")).unwrap();
        typ(&mut app, "z");
        app.tick(secs(t, 6));
        assert_eq!(read(&file).unwrap().1, b"xyzabc\n");
        std::fs::remove_dir_all(d.path().join("rec")).unwrap();
        std::fs::write(d.path().join("rec"), "").unwrap();
        typ(&mut app, "w");
        assert!(app.tick(secs(t, 9)));
    }

    #[test]
    fn clean_save_removes_recovery() {
        let d = tempdir().unwrap();
        let (p, file, mut app) = clean(&d);
        let t = Instant::now();
        typ(&mut app, "x");
        app.tick(t);
        assert!(file.exists());
        app.dispatch(CommandId::Save);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "xabc\n");
        assert!(!file.exists());
        // Dirty again: written again, at once.
        typ(&mut app, "y");
        app.tick(secs(t, 1));
        assert!(!file.exists(), "within the interval");
        app.tick(secs(t, 3));
        assert_eq!(read(&file).unwrap().1, b"xyabc\n");
        // A failed save keeps it.
        let gone = d.path().join("no-such-dir").join("b.txt");
        let mut app = open(&d, &gone, dead);
        let file = rec_path(&d.path().join("rec"), Some(&gone), 1);
        typ(&mut app, "x");
        app.tick(t);
        app.dispatch(CommandId::Save);
        assert!(status(&app).starts_with("Save failed: "));
        assert!(file.exists());
    }

    #[test]
    fn undo_to_save_point_deletes_recovery() {
        let d = tempdir().unwrap();
        let (_, file, mut app) = clean(&d);
        let t = Instant::now();
        typ(&mut app, "x");
        app.tick(t);
        assert!(file.exists());
        app.dispatch(CommandId::Undo);
        assert!(!app.editor.buf.dirty);
        app.tick(secs(t, 1));
        assert!(!file.exists());
        app.dispatch(CommandId::Redo);
        app.tick(secs(t, 3));
        assert_eq!(read(&file).unwrap().1, b"xabc\n");
    }

    fn ctrl_open(app: &mut App, p: &Path) {
        app.handle_event(ctrl('o'));
        if app.editor.buf.dirty {
            press(app, 'n');
        }
        submit_name(app, p);
    }

    #[test]
    fn lock_removed_on_quit_and_open_away() {
        let d = tempdir().unwrap();
        let dir = d.path().join("rec");
        let (a, a_rec, mut app) = clean(&d);
        let a_lock = lock_path(&dir, &a);
        let h = read(&a_lock).unwrap().0;
        assert_eq!(
            (h.pid, h.path),
            (std::process::id(), a.display().to_string())
        );
        // Open away from a clean buffer.
        let b = d.path().join("b.txt");
        std::fs::write(&b, "b\n").unwrap();
        ctrl_open(&mut app, &b);
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        assert!(!a_lock.exists());
        assert!(lock_path(&dir, &b).exists());
        // Open away after a no to saving: its recovery file goes too, and
        // reopening the same file does not offer what was just thrown away.
        typ(&mut app, "x");
        app.tick(Instant::now());
        let b_rec = rec_path(&dir, Some(&b), 1);
        assert!(b_rec.exists());
        ctrl_open(&mut app, &b);
        assert!(app.prompt.is_none(), "no recovery offer");
        assert_eq!(app.editor.buf.rope.to_string(), "b");
        assert!(!b_rec.exists() && lock_path(&dir, &b).exists());
        ctrl_open(&mut app, &a);
        assert!(!lock_path(&dir, &b).exists() && a_lock.exists());
        // Quit, clean and after a no.
        app.dispatch(CommandId::Quit);
        assert!(app.quit && !a_lock.exists());
        let mut app = open(&d, &a, dead);
        typ(&mut app, "x");
        app.tick(Instant::now());
        assert!(a_rec.exists());
        app.dispatch(CommandId::Quit);
        press(&mut app, 'n');
        assert!(app.quit && !a_lock.exists() && !a_rec.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn read_only_open_writes_no_recovery() {
        let d = tempdir().unwrap();
        let dir = d.path().join("rec");
        let a = locked(&d, "disk\n");
        let a_lock = lock_path(&dir, &a);
        let theirs = std::fs::read(&a_lock).unwrap();
        let mut app = open(&d, &a, alive);
        press(&mut app, 'y');
        typ(&mut app, "x");
        let t = Instant::now();
        app.tick(t);
        assert!(!rec_path(&dir, Some(&a), 1).exists());
        assert_eq!(std::fs::read(&a_lock).unwrap(), theirs);
        // Save As makes a file of our own, with a lock and recovery.
        let b = d.path().join("b.txt");
        app.dispatch(CommandId::SaveAs);
        while matches!(&app.prompt, Some((Prompt::Text { input, .. }, _)) if !input.is_empty()) {
            app.handle_event(key(KeyCode::Backspace, KeyModifiers::NONE));
        }
        submit_name(&mut app, &b);
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        assert!(!app.read_only);
        assert_eq!(
            read(&lock_path(&dir, &b)).unwrap().0.pid,
            std::process::id()
        );
        typ(&mut app, "y");
        app.tick(secs(t, 3));
        assert!(rec_path(&dir, Some(&b), 1).exists());
        app.dispatch(CommandId::Quit);
        press(&mut app, 'n');
        assert_eq!(std::fs::read(&a_lock).unwrap(), theirs);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn untitled_recovery_named_by_pid() {
        let d = tempdir().unwrap();
        let dir = d.path().join("rec");
        let unnamed = || {
            let mut rec = Recovery::new(dir.clone(), dead);
            rec.pid = 77;
            let mut app = App::open_with(None, None, rec).unwrap();
            typ(&mut app, "hi");
            app.tick(Instant::now());
            app
        };
        let file = dir.join("untitled-77.rec");
        // Named by a save.
        let mut app = unnamed();
        let (h, payload) = read(&file).unwrap();
        assert_eq!(
            (h.pid, h.path.as_str(), payload.as_slice()),
            (77, "", &b"hi\n"[..])
        );
        app.dispatch(CommandId::Save);
        submit_name(&mut app, &d.path().join("n.txt"));
        assert!(!file.exists());
        assert!(lock_path(&dir, &d.path().join("n.txt")).exists());
        // Replaced by an open.
        let mut app = unnamed();
        assert!(file.exists());
        let b = d.path().join("b.txt");
        std::fs::write(&b, "b\n").unwrap();
        ctrl_open(&mut app, &b);
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        assert!(!file.exists());
    }

    #[test]
    fn fatal_error_writes_recovery_when_dirty() {
        let d = tempdir().unwrap();
        let (_, file, mut app) = clean(&d);
        app.on_fatal_error();
        assert!(!file.exists());
        typ(&mut app, "x");
        app.tick(Instant::now());
        // Within the interval, but an error exit does not wait.
        typ(&mut app, "y");
        app.on_fatal_error();
        assert_eq!(read(&file).unwrap().1, b"xyabc\n");
        // A read-only copy of another nib's file still writes nothing.
        std::fs::remove_file(&file).unwrap();
        let a = locked(&d, "disk\n");
        let mut app = open(&d, &a, alive);
        press(&mut app, 'y');
        typ(&mut app, "x");
        app.on_fatal_error();
        assert!(!file.exists());
    }

    #[test]
    fn recovery_roundtrip() {
        let d = tempdir().unwrap();
        // Not there yet: the first write makes the directory.
        let dir = d.path().join("nib").join("recovery");
        let p = d.path().join("a.txt");
        let buf = Buffer::from_str_detect("\u{FEFF}one\r\ntwo\r\n");
        let file = rec_path(&dir, Some(&p), 12);
        write(&file, 12, Some(&p), &buf.to_bytes()).unwrap();
        let raw = std::fs::read(&file).unwrap();
        let head = format!("nib-recovery 1\npid 12\npath {}\n\n", p.display());
        assert!(raw.starts_with(head.as_bytes()));
        let (h, payload) = read(&file).unwrap();
        assert_eq!(h.pid, 12);
        assert_eq!(h.path, p.to_string_lossy());
        assert_eq!(payload, buf.to_bytes());
        let back = Buffer::from_str_detect(std::str::from_utf8(&payload).unwrap());
        assert_eq!(back.to_bytes(), buf.to_bytes());
    }

    #[test]
    fn path_hash_stable() {
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
        let dir = Path::new("rec");
        let p = Path::new("/home/u/a.txt");
        let want = format!("{:016x}.rec", fnv1a64(p.as_os_str().as_encoded_bytes()));
        assert_eq!(rec_path(dir, Some(p), 7), dir.join(&want));
        assert_eq!(rec_path(dir, Some(p), 8), dir.join(want));
        assert_eq!(rec_path(dir, None, 42), dir.join("untitled-42.rec"));
        let lock = lock_path(dir, p);
        assert_eq!(lock.extension().unwrap(), "lock");
        assert_eq!(lock.file_stem(), rec_path(dir, Some(p), 1).file_stem());
    }
}
