use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::commands::{COMMANDS, CommandId};
use crate::keymap::{Keymap, format_chord};

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
            Some("--version" | "-V") => return Ok(Args::Version),
            Some("--help" | "-h") => return Ok(Args::Help),
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

const HELP_USAGE: &str = "Usage: nib [OPTIONS] [FILE[:LINE]]

Arguments:
  FILE        File to edit; a new file is created on its first save
  FILE:LINE   Open FILE with the cursor on LINE

Options:
  -h, --help      Show this help
  -V, --version   Show the version
      --          Treat the next argument as a file name, even if it starts with -

Examples:
  nib                Empty buffer; the first save asks for a name
  nib notes          Edit notes.txt (a new name without an extension gets .txt)
  nib todo.md        Edit todo.md
  nib app.log:42     Open app.log at line 42
  nib -- -draft.md   Edit a file whose name starts with -
";

/// Commands listed under `Keys:`; the palette lists the rest.
const HELP_KEYS: [CommandId; 13] = [
    CommandId::Palette,
    CommandId::Save,
    CommandId::Open,
    CommandId::Quit,
    CommandId::Find,
    CommandId::Replace,
    CommandId::GotoLine,
    CommandId::Undo,
    CommandId::Redo,
    CommandId::Copy,
    CommandId::Cut,
    CommandId::Paste,
    CommandId::SelectAll,
];

/// The `--help` text. Keys come from `keymap`, so they match the user's
/// config; a path is `None` when the system has no such directory.
pub fn help_text(keymap: &Keymap, config: Option<&Path>, recovery: Option<&Path>) -> String {
    let mut out = format!(
        "{} - {}

{HELP_USAGE}
Keys:
",
        version_text(),
        env!("CARGO_PKG_DESCRIPTION")
    );
    let rows: Vec<(String, &str)> = HELP_KEYS
        .iter()
        .map(|&id| {
            let name = COMMANDS.iter().find(|c| c.id == id).map_or("", |c| c.name);
            let keys: Vec<String> = keymap.keys_for(id).iter().map(format_chord).collect();
            let keys = match (keys.is_empty(), id) {
                (false, _) => keys.join(", "),
                (true, CommandId::Palette) => "(none)".to_string(),
                (true, _) => "(palette)".to_string(),
            };
            (keys, name)
        })
        .collect();
    let width = rows.iter().map(|(keys, _)| keys.len()).max().unwrap_or(0);
    for (keys, name) in &rows {
        out.push_str(&format!(
            "  {keys:<width$}   {name}
"
        ));
    }
    out.push_str(
        "  Every other command, with its keys, is in the command palette.
",
    );
    out.push_str(
        "
Files:
",
    );
    for (label, path) in [("Config", config), ("Recovery", recovery)] {
        let shown = path.map_or("not available on this system".to_string(), |p| {
            p.display().to_string()
        });
        out.push_str(&format!(
            "  {label:<10}{shown}
"
        ));
    }
    out.push_str(
        "  The config sets wrap, line_numbers, tab_width, line_ending and [keys].
",
    );
    out.push_str(concat!(
        "
Docs: ",
        env!("CARGO_PKG_REPOSITORY"),
        "#readme"
    ));
    out
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
        assert_eq!(parse_strs(&["-V"]), Ok(Args::Version));
        assert_eq!(parse_strs(&["--help"]), Ok(Args::Help));
        assert_eq!(parse_strs(&["-h"]), Ok(Args::Help));
        assert_eq!(version_text(), "nib 0.1.0");
    }

    #[test]
    fn short_flags_after_double_dash_are_paths() {
        assert_eq!(
            parse_strs(&["--", "-h"]),
            Ok(Args::Edit {
                path: Some(PathBuf::from("-h")),
                line: None
            })
        );
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

    fn default_help() -> String {
        help_text(
            &Keymap::defaults(),
            Some(Path::new("cfg/nib/config.toml")),
            Some(Path::new("data/nib/recovery")),
        )
    }

    #[test]
    fn help_covers_usage_options_examples_keys_files() {
        let help = default_help();
        assert!(help.starts_with("nib 0.1.0 - "));
        for needle in [
            "Usage: nib [OPTIONS] [FILE[:LINE]]",
            "-h, --help",
            "-V, --version",
            "nib app.log:42",
            "Ctrl+P",
            "Command Palette",
            "Ctrl+Y, Ctrl+Shift+Z",
            "cfg/nib/config.toml",
            "data/nib/recovery",
            "https://github.com/StrandedDev/nib#readme",
        ] {
            assert!(
                help.contains(needle),
                "missing {needle:?} in:
{help}"
            );
        }
    }

    #[test]
    fn help_keys_follow_keymap() {
        let mut km = Keymap::defaults();
        km.set_keys(
            CommandId::Save,
            &[crate::keymap::parse_chord("f2").unwrap()],
        );
        km.set_keys(CommandId::Find, &[]);
        let help = help_text(&km, None, None);
        let line = |name: &str| {
            help.lines()
                .find(|l| l.trim_end().ends_with(name))
                .unwrap_or_else(|| panic!("no {name} row"))
                .to_string()
        };
        assert!(line(" Save").trim_start().starts_with("F2 "));
        assert!(line(" Find").trim_start().starts_with("(palette) "));
        assert!(help.contains("not available on this system"));
    }

    #[test]
    fn help_rows_aligned_without_trailing_space() {
        let help = default_help();
        for line in help.lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace: {line:?}");
        }
        let key_rows: Vec<&str> = help
            .lines()
            .skip_while(|l| *l != "Keys:")
            .skip(1)
            .take(HELP_KEYS.len())
            .collect();
        let name_col = |l: &str| l.rfind("   ").unwrap();
        let col = name_col(key_rows[0]);
        assert!(key_rows.iter().all(|l| name_col(l) == col), "{key_rows:#?}");
    }
}
