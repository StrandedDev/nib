//! Key chords and the default keymap, built from the command registry.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::commands::{COMMANDS, CommandId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

/// One canonical form for matching: lowercase letters with SHIFT, BackTab as Shift+Tab,
/// only Ctrl/Shift/Alt kept, SHIFT dropped on non-letter characters.
fn normalise(code: KeyCode, mods: KeyModifiers) -> KeyChord {
    let mut mods = mods & (KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT);
    let code = match code {
        KeyCode::BackTab => {
            mods |= KeyModifiers::SHIFT;
            KeyCode::Tab
        }
        KeyCode::Char(c) if c.is_ascii_uppercase() => {
            mods |= KeyModifiers::SHIFT;
            KeyCode::Char(c.to_ascii_lowercase())
        }
        KeyCode::Char(c) => {
            if !c.is_alphabetic() {
                mods -= KeyModifiers::SHIFT;
            }
            KeyCode::Char(c)
        }
        other => other,
    };
    KeyChord { code, mods }
}

pub fn parse_chord(s: &str) -> Result<KeyChord, String> {
    let mut mods = KeyModifiers::NONE;
    let mut key = None;
    for part in s.split('+') {
        let lower = part.to_lowercase();
        match lower.as_str() {
            "ctrl" => mods |= KeyModifiers::CONTROL,
            "shift" => mods |= KeyModifiers::SHIFT,
            "alt" => mods |= KeyModifiers::ALT,
            _ => {
                if key.replace(parse_key(part, &lower)?).is_some() {
                    return Err(format!("more than one key in '{s}'"));
                }
            }
        }
    }
    let code = key.ok_or_else(|| format!("no key in '{s}'"))?;
    Ok(normalise(code, mods))
}

fn parse_key(raw: &str, lower: &str) -> Result<KeyCode, String> {
    let code = match lower {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "space" => KeyCode::Char(' '),
        _ => {
            if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
                && (1..=12).contains(&n)
            {
                return Ok(KeyCode::F(n));
            }
            let mut chars = raw.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(KeyCode::Char(c)),
                _ => Err(format!("unknown key '{raw}'")),
            };
        }
    };
    Ok(code)
}

/// Display form such as `Ctrl+Shift+Z`; modifier order is Ctrl, Alt, Shift.
pub fn format_chord(chord: &KeyChord) -> String {
    let mut out = String::new();
    for (flag, name) in [
        (KeyModifiers::CONTROL, "Ctrl+"),
        (KeyModifiers::ALT, "Alt+"),
        (KeyModifiers::SHIFT, "Shift+"),
    ] {
        if chord.mods.contains(flag) {
            out.push_str(name);
        }
    }
    match chord.code {
        KeyCode::Char(' ') => out.push_str("Space"),
        KeyCode::Char(c) => out.extend(c.to_uppercase()),
        KeyCode::F(n) => out.push_str(&format!("F{n}")),
        KeyCode::Enter => out.push_str("Enter"),
        KeyCode::Esc => out.push_str("Esc"),
        KeyCode::Tab => out.push_str("Tab"),
        KeyCode::Backspace => out.push_str("Backspace"),
        KeyCode::Delete => out.push_str("Delete"),
        KeyCode::Insert => out.push_str("Insert"),
        KeyCode::Home => out.push_str("Home"),
        KeyCode::End => out.push_str("End"),
        KeyCode::PageUp => out.push_str("PageUp"),
        KeyCode::PageDown => out.push_str("PageDown"),
        KeyCode::Up => out.push_str("Up"),
        KeyCode::Down => out.push_str("Down"),
        KeyCode::Left => out.push_str("Left"),
        KeyCode::Right => out.push_str("Right"),
        other => out.push_str(&format!("{other:?}")),
    }
    out
}

pub struct Keymap {
    map: HashMap<KeyChord, CommandId>,
}

impl Keymap {
    pub fn defaults() -> Keymap {
        let mut map = HashMap::new();
        for c in COMMANDS {
            for k in c.default_keys.iter().chain(c.hidden_keys) {
                let chord = parse_chord(k).expect("default chord parses");
                map.insert(chord, c.id);
            }
        }
        Keymap { map }
    }

    pub fn lookup(&self, ev: &KeyEvent) -> Option<CommandId> {
        if ev.kind == KeyEventKind::Release {
            return None;
        }
        self.map.get(&normalise(ev.code, ev.modifiers)).copied()
    }

    /// Binds `chord` to `id`, replacing any existing binding of that chord.
    pub fn bind(&mut self, chord: KeyChord, id: CommandId) {
        self.map.insert(chord, id);
    }

    /// Visible chords bound to `id`: registry-declared defaults first, in order, then any
    /// others added with `bind` (sorted by display form). Hidden defaults are omitted.
    pub fn keys_for(&self, id: CommandId) -> Vec<KeyChord> {
        let parse = |keys: &[&str]| -> Vec<KeyChord> {
            keys.iter().filter_map(|k| parse_chord(k).ok()).collect()
        };
        let (declared, hidden) = COMMANDS
            .iter()
            .find(|c| c.id == id)
            .map(|c| (parse(c.default_keys), parse(c.hidden_keys)))
            .unwrap_or_default();
        let mut out: Vec<KeyChord> = declared
            .iter()
            .copied()
            .filter(|ch| self.map.get(ch) == Some(&id))
            .collect();
        let mut extra: Vec<KeyChord> = self
            .map
            .iter()
            .filter(|&(ch, &cid)| cid == id && !declared.contains(ch) && !hidden.contains(ch))
            .map(|(ch, _)| *ch)
            .collect();
        extra.sort_by_key(format_chord);
        out.extend(extra);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandId;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    #[test]
    fn default_save_is_ctrl_s() {
        let km = Keymap::defaults();
        let ev = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(km.lookup(&ev), Some(CommandId::Save));
        assert_eq!(
            km.keys_for(CommandId::Save),
            vec![parse_chord("ctrl+s").unwrap()]
        );
    }

    #[test]
    fn parse_format_roundtrip() {
        for (src, shown) in [
            ("ctrl+shift+z", "Ctrl+Shift+Z"),
            ("f1", "F1"),
            ("ctrl+home", "Ctrl+Home"),
            ("pageup", "PageUp"),
            ("space", "Space"),
            ("alt+shift+Left", "Alt+Shift+Left"),
        ] {
            let chord = parse_chord(src).unwrap();
            assert_eq!(format_chord(&chord), shown);
            assert_eq!(parse_chord(shown).unwrap(), chord);
        }
        assert!(parse_chord("ctrl+").is_err());
        assert!(parse_chord("ctrl+wat").is_err());
    }

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn shift_letter_normalised() {
        let km = Keymap::defaults();
        let want = Some(CommandId::Redo);
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(km.lookup(&ev(KeyCode::Char('Z'), ctrl_shift)), want);
        assert_eq!(km.lookup(&ev(KeyCode::Char('z'), ctrl_shift)), want);
        assert_eq!(
            km.lookup(&ev(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            Some(CommandId::Undo)
        );
        assert_eq!(
            km.lookup(&ev(KeyCode::BackTab, KeyModifiers::NONE)),
            km.lookup(&ev(KeyCode::Tab, KeyModifiers::SHIFT))
        );
        assert_eq!(parse_chord("shift+!").unwrap(), parse_chord("!").unwrap());
        let super_mod = KeyModifiers::CONTROL | KeyModifiers::SUPER;
        assert_eq!(
            km.lookup(&ev(KeyCode::Char('s'), super_mod)),
            Some(CommandId::Save)
        );
    }

    #[test]
    fn ctrl_letter_from_terminal_matches() {
        let km = Keymap::defaults();
        let e = ev(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(km.lookup(&e), Some(CommandId::Save));
    }

    #[test]
    fn release_events_ignored() {
        let km = Keymap::defaults();
        let mut e = ev(KeyCode::Char('s'), KeyModifiers::CONTROL);
        e.kind = KeyEventKind::Release;
        assert_eq!(km.lookup(&e), None);
        e.kind = KeyEventKind::Repeat;
        assert_eq!(km.lookup(&e), Some(CommandId::Save));
    }

    #[test]
    fn redo_has_two_keys() {
        let km = Keymap::defaults();
        let keys: Vec<String> = km
            .keys_for(CommandId::Redo)
            .iter()
            .map(format_chord)
            .collect();
        assert_eq!(keys, ["Ctrl+Y", "Ctrl+Shift+Z"]);
    }

    #[test]
    fn f1_hidden_in_keys_for() {
        let km = Keymap::defaults();
        assert_eq!(
            km.lookup(&ev(KeyCode::F(1), KeyModifiers::NONE)),
            Some(CommandId::Palette)
        );
        assert_eq!(
            km.keys_for(CommandId::Palette),
            vec![parse_chord("ctrl+p").unwrap()]
        );
    }

    #[test]
    fn palette_only_commands_have_no_keys() {
        let km = Keymap::defaults();
        for id in [
            CommandId::SaveAs,
            CommandId::ToggleWrap,
            CommandId::ToggleLineNumbers,
            CommandId::FindNext,
            CommandId::FindPrev,
        ] {
            assert!(km.keys_for(id).is_empty(), "{id:?}");
        }
    }

    #[test]
    fn bind_replaces_existing_chord() {
        let mut km = Keymap::defaults();
        let chord = parse_chord("ctrl+s").unwrap();
        km.bind(chord, CommandId::SaveAs);
        let e = ev(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(km.lookup(&e), Some(CommandId::SaveAs));
        assert_eq!(km.keys_for(CommandId::SaveAs), vec![chord]);
        assert!(km.keys_for(CommandId::Save).is_empty());
    }
}
