//! The TOML config file: settings and keymap overrides.

use std::collections::{BTreeMap, HashMap};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::buffer::LineEnding;
use crate::commands::COMMANDS;
use crate::keymap::{KeyChord, Keymap, format_chord, parse_chord};

#[derive(Debug, PartialEq)]
pub struct Config {
    pub wrap: bool,
    pub line_numbers: bool,
    /// Overrides the detected indent width when set.
    pub tab_width: Option<u8>,
    /// Line ending for new files.
    pub line_ending: LineEnding,
    pub keymap: Keymap,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            wrap: true,
            line_numbers: false,
            tab_width: None,
            line_ending: LineEnding::Lf,
            keymap: Keymap::defaults(),
        }
    }
}

/// `<config dir>/nib/config.toml`: `~/.config` on Linux, `%APPDATA%` on
/// Windows. `None` when the system has no config directory.
pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("nib").join("config.toml"))
}

/// Reads the config at `path()`.
pub fn load() -> (Config, Option<String>) {
    match path() {
        Some(path) => load_from(&path),
        None => (Config::default(), None),
    }
}

/// Reads the config at `path`. A missing file gives the defaults; anything
/// wrong with it gives the defaults and a warning for the status line.
pub fn load_from(path: &Path) -> (Config, Option<String>) {
    let parsed = match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == ErrorKind::NotFound => return (Config::default(), None),
        Err(e) => Err(e.to_string()),
    };
    match parsed {
        Ok(cfg) => (cfg, None),
        Err(msg) => (
            Config::default(),
            Some(format!("Config error: {msg} \u{2014} using defaults")),
        ),
    }
}

/// Largest `tab_width` accepted; detection never picks more than 8 either.
const MAX_TAB_WIDTH: u8 = 8;

/// The file as written. Every field is optional; unknown fields are errors.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    wrap: Option<bool>,
    line_numbers: Option<bool>,
    tab_width: Option<u8>,
    line_ending: Option<RawLineEnding>,
    #[serde(default)]
    keys: BTreeMap<String, Keys>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawLineEnding {
    Lf,
    Crlf,
}

/// A `[keys]` value: one chord or a list of them.
#[derive(Deserialize)]
#[serde(untagged)]
enum Keys {
    One(String),
    Many(Vec<String>),
}

impl Keys {
    fn as_slice(&self) -> &[String] {
        match self {
            Keys::One(k) => std::slice::from_ref(k),
            Keys::Many(ks) => ks,
        }
    }
}

fn parse(text: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| toml_error(text, &e))?;
    let mut cfg = Config::default();
    if let Some(wrap) = raw.wrap {
        cfg.wrap = wrap;
    }
    if let Some(n) = raw.line_numbers {
        cfg.line_numbers = n;
    }
    if let Some(n) = raw.tab_width {
        if !(1..=MAX_TAB_WIDTH).contains(&n) {
            return Err(format!("tab_width must be 1 to {MAX_TAB_WIDTH}"));
        }
        cfg.tab_width = Some(n);
    }
    if let Some(le) = raw.line_ending {
        cfg.line_ending = match le {
            RawLineEnding::Lf => LineEnding::Lf,
            RawLineEnding::Crlf => LineEnding::CrLf,
        };
    }
    // Chord -> config key that claimed it; keys are visited in sorted order.
    let mut claimed: HashMap<KeyChord, &str> = HashMap::new();
    for (name, keys) in &raw.keys {
        let cmd = COMMANDS
            .iter()
            .find(|c| c.config_key == name)
            .ok_or_else(|| format!("unknown command '{name}'"))?;
        let chords = keys
            .as_slice()
            .iter()
            .map(|k| parse_chord(k))
            .collect::<Result<Vec<_>, _>>()?;
        for &chord in &chords {
            if let Some(other) = claimed.insert(chord, name) {
                if other != name {
                    let chord = format_chord(&chord);
                    return Err(format!("{chord} is set for both {other} and {name}"));
                }
            }
        }
        cfg.keymap.set_keys(cmd.id, &chords);
    }
    Ok(cfg)
}

/// One line for the status bar: `line N: <message>`.
fn toml_error(text: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let before = &text.as_bytes()[..span.start.min(text.len())];
            let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandId;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tempfile::tempdir;

    #[test]
    fn missing_file_defaults_no_warning() {
        let d = tempdir().unwrap();
        let (cfg, warning) = load_from(&d.path().join("config.toml"));
        assert_eq!(warning, None);
        assert!(cfg.wrap);
        assert!(!cfg.line_numbers);
        assert_eq!(cfg.tab_width, None);
        assert_eq!(cfg.line_ending, LineEnding::Lf);
    }

    fn load_text(text: &str) -> (Config, Option<String>) {
        let d = tempdir().unwrap();
        let p = d.path().join("config.toml");
        std::fs::write(&p, text).unwrap();
        load_from(&p)
    }

    #[test]
    fn bad_toml_defaults_with_warning() {
        let (cfg, warning) = load_text("wrap = false\nline_numbers = \n");
        // The middle is toml's own wording, which can change between versions.
        let warning = warning.expect("a warning");
        assert!(warning.starts_with("Config error: line 2: "), "{warning:?}");
        assert!(warning.ends_with(" \u{2014} using defaults"), "{warning:?}");
        assert!(!warning.contains('\n'), "{warning:?}");
        assert!(cfg.wrap);
        assert_eq!(cfg, Config::default());
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn override_key() {
        let (cfg, warning) = load_text("[keys]\nsave = \"ctrl+w\"\n");
        assert_eq!(warning, None);
        assert_eq!(cfg.keymap.lookup(&ctrl('w')), Some(CommandId::Save));
        assert_eq!(cfg.keymap.lookup(&ctrl('s')), None);
    }

    #[test]
    fn override_steals_chord() {
        let (cfg, warning) = load_text("[keys]\nsave = \"ctrl+f\"\n");
        assert_eq!(warning, None);
        assert_eq!(cfg.keymap.lookup(&ctrl('f')), Some(CommandId::Save));
        assert_eq!(cfg.keymap.keys_for(CommandId::Find), vec![]);
    }

    #[test]
    fn list_of_keys() {
        let (cfg, warning) = load_text("[keys]\nundo = [\"ctrl+u\", \"alt+z\"]\n");
        assert_eq!(warning, None);
        assert_eq!(cfg.keymap.lookup(&ctrl('u')), Some(CommandId::Undo));
        let alt_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
        assert_eq!(cfg.keymap.lookup(&alt_z), Some(CommandId::Undo));
        assert_eq!(cfg.keymap.lookup(&ctrl('z')), None);
    }

    #[test]
    fn unknown_setting_command_or_chord_warns() {
        for (text, msg) in [
            (
                "colour = \"red\"\n",
                "line 1: unknown field `colour`, expected one of `wrap`, `line_numbers`, `tab_width`, `line_ending`, `keys`",
            ),
            ("[keys]\nsav = \"ctrl+s\"\n", "unknown command 'sav'"),
            ("[keys]\nsave = \"ctrl+wat\"\n", "unknown key 'wat'"),
        ] {
            let (cfg, warning) = load_text(text);
            let want = format!("Config error: {msg} \u{2014} using defaults");
            assert_eq!(warning.as_deref(), Some(want.as_str()), "{text:?}");
            assert_eq!(cfg.keymap.lookup(&ctrl('s')), Some(CommandId::Save));
        }
    }

    #[test]
    fn same_chord_for_two_commands_warns() {
        let (_, warning) =
            load_text("[keys]\nsave = \"ctrl+k\"\nquit = [\"ctrl+q\", \"ctrl+k\"]\n");
        assert_eq!(
            warning.as_deref(),
            Some("Config error: Ctrl+K is set for both quit and save \u{2014} using defaults")
        );
    }
}
