use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::buffer::Buffer;

pub const LARGE_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_LINK_HOPS: usize = 40;
const MAX_TEMP_RETRIES: u32 = 99;
const BINARY_SNIFF_BYTES: usize = 8192;

pub enum Loaded {
    Text(Buffer),
    NotFound,
    Directory,
    Binary,
    TooLarge(u64),
}

pub fn load(path: &Path, allow_large: bool) -> io::Result<Loaded> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Loaded::NotFound),
        Err(e) => return Err(e),
    };
    if meta.is_dir() {
        return Ok(Loaded::Directory);
    }
    if !allow_large && meta.len() > LARGE_FILE_BYTES {
        return Ok(Loaded::TooLarge(meta.len()));
    }
    let bytes = fs::read(path)?;
    if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return Ok(Loaded::Binary);
    }
    match String::from_utf8(bytes) {
        Ok(s) => Ok(Loaded::Text(Buffer::from_str_detect(&s))),
        Err(_) => Ok(Loaded::Binary),
    }
}

fn resolve_target(path: &Path) -> io::Result<PathBuf> {
    let mut cur = path.to_path_buf();
    for _ in 0..=MAX_LINK_HOPS {
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                let link = fs::read_link(&cur)?;
                cur = cur.parent().unwrap_or(Path::new("")).join(link);
            }
            _ => return Ok(cur),
        }
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

/// Directory that holds `target`; a bare relative name maps to `.` so the
/// directory fsync after rename still runs.
pub(crate) fn save_dir(target: &Path) -> &Path {
    match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = resolve_target(path)?;
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let dir = save_dir(&target);
    let pid = std::process::id();

    // Only a temp file this call created is ever removed.
    let mut created = None;
    for n in 0..=MAX_TEMP_RETRIES {
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(name);
        if n == 0 {
            tmp_name.push(format!(".nib-{pid}.tmp"));
        } else {
            tmp_name.push(format!(".nib-{pid}-{n}.tmp"));
        }
        let tmp = dir.join(tmp_name);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => {
                created = Some((tmp, f));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    let Some((tmp, f)) = created else {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not find a free temp file name",
        ));
    };

    let result = write_and_rename(f, &tmp, &target, dir, bytes);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_and_rename(
    mut f: fs::File,
    tmp: &Path,
    target: &Path,
    dir: &Path,
    bytes: &[u8],
) -> io::Result<()> {
    let original = fs::metadata(target).ok();
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    if let Some(m) = &original {
        fs::set_permissions(tmp, m.permissions())?;
        #[cfg(unix)]
        keep_owner(tmp, m);
    }
    fs::rename(tmp, target)?;
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(unix)]
fn keep_owner(tmp: &Path, original: &fs::Metadata) {
    use std::os::unix::fs::{MetadataExt, chown};
    let Ok(t) = fs::metadata(tmp) else { return };
    if t.uid() != original.uid() || t.gid() != original.gid() {
        // EPERM when not privileged is expected and ignored
        let _ = chown(tmp, Some(original.uid()), Some(original.gid()));
    }
}

pub fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

pub fn is_writable(path: &Path) -> bool {
    if path.exists() {
        return OpenOptions::new().write(true).open(path).is_ok();
    }
    let probe = save_dir(path).join(format!(".nib-probe-{}", std::process::id()));
    let ok = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();
    if ok {
        let _ = fs::remove_file(&probe);
    }
    ok
}

pub fn format_mtime(t: SystemTime) -> String {
    jiff::Timestamp::try_from(t)
        .map(|ts| {
            ts.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn text(l: Loaded) -> Buffer {
        match l {
            Loaded::Text(b) => b,
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn load_text() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "hi\nthere\n").unwrap();
        let b = text(load(&p, false).unwrap());
        assert_eq!(b.rope.to_string(), "hi\nthere");
        assert!(b.meta.final_newline);
    }

    #[test]
    fn load_missing_is_notfound() {
        let d = tempdir().unwrap();
        let r = load(&d.path().join("nope"), false).unwrap();
        assert!(matches!(r, Loaded::NotFound));
    }

    #[test]
    fn load_dir() {
        let d = tempdir().unwrap();
        assert!(matches!(load(d.path(), false).unwrap(), Loaded::Directory));
    }

    #[test]
    fn load_nul_is_binary() {
        let d = tempdir().unwrap();
        let p = d.path().join("b");
        fs::write(&p, b"ab\0cd").unwrap();
        assert!(matches!(load(&p, false).unwrap(), Loaded::Binary));
    }

    #[test]
    fn load_bad_utf8_is_binary() {
        let d = tempdir().unwrap();
        let p = d.path().join("b");
        let mut bytes = vec![b'a'; 10000];
        bytes.push(0xFF);
        fs::write(&p, bytes).unwrap();
        assert!(matches!(load(&p, false).unwrap(), Loaded::Binary));
    }

    #[test]
    fn load_large_needs_flag() {
        let d = tempdir().unwrap();
        let p = d.path().join("big");
        let len = 51 * 1024 * 1024;
        fs::File::create(&p).unwrap().set_len(len).unwrap();
        match load(&p, false).unwrap() {
            Loaded::TooLarge(n) => assert_eq!(n, len),
            _ => panic!("expected TooLarge"),
        }
        // all NULs, so with the flag it is rejected as binary rather than size
        assert!(matches!(load(&p, true).unwrap(), Loaded::Binary));
    }

    #[test]
    fn load_large_with_flag_gives_text() {
        let d = tempdir().unwrap();
        let p = d.path().join("big.txt");
        fs::write(&p, vec![b'a'; 51 * 1024 * 1024]).unwrap();
        assert!(matches!(load(&p, false).unwrap(), Loaded::TooLarge(_)));
        assert!(matches!(load(&p, true).unwrap(), Loaded::Text(_)));
    }

    #[test]
    fn atomic_write_replaces_content() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "old").unwrap();
        atomic_write(&p, b"new content").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new content");
        let names: Vec<_> = fs::read_dir(d.path()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn atomic_write_bare_relative_name() {
        assert_eq!(save_dir(Path::new("a.txt")), Path::new("."));
        assert_eq!(save_dir(Path::new("d/a.txt")), Path::new("d"));
    }

    #[test]
    fn atomic_write_creates_missing_file() {
        let d = tempdir().unwrap();
        let p = d.path().join("new.txt");
        atomic_write(&p, b"x").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"x");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_follows_symlink() {
        let d = tempdir().unwrap();
        let real = d.path().join("real.txt");
        let link = d.path().join("link.txt");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        atomic_write(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_dangling_symlink_creates_target() {
        let d = tempdir().unwrap();
        let real = d.path().join("real.txt");
        let link = d.path().join("link.txt");
        std::os::unix::fs::symlink("real.txt", &link).unwrap();
        atomic_write(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"new");
    }

    #[test]
    fn existing_temp_name_is_not_deleted() {
        let d = tempdir().unwrap();
        let p = d.path().join("b.txt");
        fs::write(&p, "old").unwrap();
        let foreign = d
            .path()
            .join(format!(".b.txt.nib-{}.tmp", std::process::id()));
        fs::write(&foreign, "keep").unwrap();
        atomic_write(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        assert_eq!(fs::read(&foreign).unwrap(), b"keep");
        let tmps = fs::read_dir(d.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".nib-")
            })
            .count();
        assert_eq!(tmps, 1);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "old").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write(&p, b"new").unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    fn no_temp_left(dir: &Path) {
        for e in fs::read_dir(dir).unwrap() {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!(n.contains(".nib-") && n.ends_with(".tmp")), "left {n}");
        }
    }

    #[test]
    fn failed_write_leaves_original_and_no_temp() {
        let d = tempdir().unwrap();
        let target = d.path().join("adir");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inner.txt"), "keep").unwrap();
        assert!(atomic_write(&target, b"new").is_err());
        assert_eq!(fs::read(target.join("inner.txt")).unwrap(), b"keep");
        no_temp_left(d.path());
    }

    #[cfg(unix)]
    #[test]
    fn failed_write_in_readonly_dir_leaves_original() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "orig").unwrap();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let r = atomic_write(&p, b"new");
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o755)).unwrap();
        // root ignores directory permissions
        if r.is_err() {
            assert_eq!(fs::read(&p).unwrap(), b"orig");
        }
        no_temp_left(d.path());
    }

    #[test]
    fn writable_and_mtime() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.txt");
        assert!(is_writable(&p));
        assert!(mtime(&p).is_none());
        fs::write(&p, "x").unwrap();
        assert!(is_writable(&p));
        assert!(mtime(&p).is_some());
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p, perm).unwrap();
        assert!(!is_writable(&p));
        let mut perm = fs::metadata(&p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&p, perm).unwrap();
        assert!(!is_writable(&d.path().join("no-such-dir").join("f")));
    }

    #[test]
    fn format_mtime_shape() {
        let s = format_mtime(SystemTime::now());
        let b = s.as_bytes();
        assert_eq!(b.len(), 16, "{s}");
        for (i, c) in b.iter().enumerate() {
            match i {
                4 | 7 => assert_eq!(*c, b'-', "{s}"),
                10 => assert_eq!(*c, b' ', "{s}"),
                13 => assert_eq!(*c, b':', "{s}"),
                _ => assert!(c.is_ascii_digit(), "{s}"),
            }
        }
    }
}
