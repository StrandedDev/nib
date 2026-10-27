use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Args {
    Edit {
        path: Option<PathBuf>,
        line: Option<usize>,
    },
    Version,
    Help,
}

/// Classifies arguments only. The path is returned as typed (`line` is
/// `None`); the caller runs it through `resolve` with a real `exists`.
pub fn parse(argv: impl Iterator<Item = OsString>) -> Result<Args, String> {
    let mut path: Option<PathBuf> = None;
    let mut only_paths = false;
    for arg in argv {
        let flag = if only_paths { None } else { arg.to_str() };
        match flag {
            Some("--version") => return Ok(Args::Version),
            Some("--help") => return Ok(Args::Help),
            Some("--") => only_paths = true,
            Some(f) if f.starts_with('-') && f != "-" => {
                return Err(format!("unknown option: {f}"));
            }
            _ => {
                if path.replace(PathBuf::from(arg)).is_some() {
                    return Err("only one file may be given".to_string());
                }
            }
        }
    }
    Ok(Args::Edit { path, line: None })
}

pub fn version_text() -> String {
    format!("nib {}", env!("CARGO_PKG_VERSION"))
}

pub fn help_text() -> &'static str {
    "Usage: nib [FILE[:LINE]]
     
     A small terminal text editor.
     
     Options:
       --help      Show this help
       --version   Show the version
       --          Treat the next argument as a file name
     
     A new file without an extension gets .txt appended.
     It is created on first save."
}

pub fn resolve(raw: &str, exists: impl Fn(&Path) -> bool) -> (PathBuf, Option<usize>) {
    if exists(Path::new(raw)) {
        return (PathBuf::from(raw), None);
    }
    let (name, line) = match raw.rsplit_once(':') {
        Some((before, digits))
            if !before.is_empty()
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            match digits.parse::<usize>() {
                Ok(n) => (before, Some(n)),
                Err(_) => (raw, None),
            }
        }
        _ => (raw, None),
    };
    if exists(Path::new(name)) {
        (PathBuf::from(name), line)
    } else {
        (PathBuf::from(new_file_name(name)), line)
    }
}

const KNOWN_EXTENSIONLESS: [&str; 4] = ["Makefile", "Dockerfile", "LICENSE", "README"];

pub fn new_file_name(raw: &str) -> String {
    let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let keep = name.starts_with('.')
        || name.contains('.')
        || KNOWN_EXTENSIONLESS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(name));
    if keep {
        raw.to_string()
    } else {
        format!("{raw}.txt")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_gets_txt() {
        assert_eq!(new_file_name("todo"), "todo.txt");
    }

    #[test]
    fn ext_kept() {
        assert_eq!(new_file_name("a.md"), "a.md");
    }

    #[test]
    fn dotfile_kept() {
        assert_eq!(new_file_name(".rc"), ".rc");
        assert_eq!(new_file_name("dir/.rc"), "dir/.rc");
    }

    #[test]
    fn known_extensionless_kept() {
        for n in [
            "Makefile",
            "Dockerfile",
            "LICENSE",
            "README",
            "makefile",
            "dir/Makefile",
        ] {
            assert_eq!(new_file_name(n), n);
        }
    }

    #[test]
    fn dot_in_dir_name_ignored() {
        assert_eq!(new_file_name("notes.d/todo"), "notes.d/todo.txt");
        assert_eq!(new_file_name(r"notes.d\todo"), r"notes.d\todo.txt");
    }

    #[test]
    fn existing_never_renamed() {
        assert_eq!(resolve("todo", |_| true), (PathBuf::from("todo"), None));
    }

    #[test]
    fn line_suffix() {
        let (p, l) = resolve("a.md:42", |_| false);
        assert_eq!((p, l), (PathBuf::from("a.md"), Some(42)));
    }

    #[test]
    fn full_name_exists_wins() {
        let (p, l) = resolve("a:42", |p| p == Path::new("a:42"));
        assert_eq!((p, l), (PathBuf::from("a:42"), None));
    }

    #[test]
    fn drive_letter_not_line_suffix() {
        let (p, l) = resolve(r"C:\x.md", |_| false);
        assert_eq!((p, l), (PathBuf::from(r"C:\x.md"), None));
        let (p, l) = resolve(r"C:\x.md:12", |_| false);
        assert_eq!((p, l), (PathBuf::from(r"C:\x.md"), Some(12)));
    }

    #[test]
    fn new_file_gets_txt_with_line() {
        let (p, l) = resolve("todo:3", |_| false);
        assert_eq!((p, l), (PathBuf::from("todo.txt"), Some(3)));
    }

    #[test]
    fn split_target_existing_not_renamed() {
        let (p, l) = resolve("todo:3", |p| p == Path::new("todo"));
        assert_eq!((p, l), (PathBuf::from("todo"), Some(3)));
    }

    fn parse_strs(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn version_and_help_flags() {
        assert_eq!(parse_strs(&["--version"]), Ok(Args::Version));
        assert_eq!(parse_strs(&["--help"]), Ok(Args::Help));
        assert_eq!(version_text(), "nib 0.1.0");
        assert!(help_text().lines().count() < 15);
    }

    #[test]
    fn no_args_is_empty_edit() {
        assert_eq!(
            parse_strs(&[]),
            Ok(Args::Edit {
                path: None,
                line: None
            })
        );
    }

    #[test]
    fn single_path_kept_raw() {
        assert_eq!(
            parse_strs(&["todo:7"]),
            Ok(Args::Edit {
                path: Some(PathBuf::from("todo:7")),
                line: None
            })
        );
    }

    #[test]
    fn unknown_flag_and_extra_paths_error() {
        assert!(parse_strs(&["-x"]).is_err());
        assert!(parse_strs(&["a", "b"]).is_err());
    }

    #[test]
    fn double_dash_makes_path() {
        assert_eq!(
            parse_strs(&["--", "-weird.md"]),
            Ok(Args::Edit {
                path: Some(PathBuf::from("-weird.md")),
                line: None
            })
        );
    }
}
