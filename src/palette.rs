//! Command palette: fuzzy list over the command registry.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::commands::{COMMANDS, CommandId};
use crate::keymap::{Keymap, format_chord};

impl App {
    /// Routes a key to the open palette; a chosen command runs once it is closed.
    pub(crate) fn palette_key(&mut self, key: &KeyEvent) {
        let Some(p) = &mut self.palette else {
            return;
        };
        match p.handle_key(key, &self.keymap) {
            PaletteOutcome::Pending => {}
            PaletteOutcome::Close => self.palette = None,
            PaletteOutcome::Run(id) => {
                self.palette = None;
                self.dispatch(id);
            }
        }
    }
}

/// One palette row.
pub struct Item {
    pub id: CommandId,
    pub name: &'static str,
    /// Formatted chords, comma-separated; empty when unbound.
    pub keys: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PaletteOutcome {
    Pending,
    Run(CommandId),
    Close,
}

#[derive(Default)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
}

impl Palette {
    /// Edits the query, moves the selection or finishes. Other keys do nothing.
    pub fn handle_key(&mut self, ev: &KeyEvent, keymap: &Keymap) -> PaletteOutcome {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let plain = !ev
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let items = self.items(keymap);
        match ev.code {
            KeyCode::Esc => return PaletteOutcome::Close,
            KeyCode::Enter => {
                return items
                    .get(self.selected)
                    .map_or(PaletteOutcome::Pending, |i| PaletteOutcome::Run(i.id));
            }
            // Ctrl+P / Ctrl+N mirror Up / Down, a common palette habit.
            KeyCode::Up => self.step(items.len(), false),
            KeyCode::Down => self.step(items.len(), true),
            KeyCode::Char('p') if ctrl => self.step(items.len(), false),
            KeyCode::Char('n') if ctrl => self.step(items.len(), true),
            KeyCode::Backspace if plain => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char(c) if plain => {
                self.query.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        PaletteOutcome::Pending
    }

    fn step(&mut self, len: usize, down: bool) {
        if len > 0 {
            self.selected = if down {
                (self.selected + 1) % len
            } else {
                (self.selected + len - 1) % len
            };
        }
    }

    /// Palette commands matching the query, best first, then by name.
    pub fn items(&self, keymap: &Keymap) -> Vec<Item> {
        let mut scored: Vec<(i32, Item)> = COMMANDS
            .iter()
            .filter(|c| c.in_palette)
            .filter_map(|c| {
                let s = score(&self.query, c.name)?;
                let keys = keymap.keys_for(c.id).into_iter().map(|k| format_chord(&k));
                let keys = keys.collect::<Vec<_>>().join(", ");
                Some((
                    s,
                    Item {
                        id: c.id,
                        name: c.name,
                        keys,
                    },
                ))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(b.1.name)));
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

/// Match score of `query` against `name`, or `None` when `query` is not a
/// case-insensitive subsequence of it. Greedy left-most match.
pub fn score(query: &str, name: &str) -> Option<i32> {
    let mut q = query.chars().flat_map(char::to_lowercase).peekable();
    let (mut total, mut matched, mut prev_hit, mut last) = (0, 0, false, 0);
    let mut prev = ' ';
    for (i, c) in name.chars().enumerate() {
        let hit = q
            .peek()
            .is_some_and(|&w| c.to_lowercase().next() == Some(w));
        if hit {
            q.next();
            if prev_hit {
                total += 10;
            }
            if prev == ' ' {
                total += 8;
            }
            matched += 1;
            last = i + 1;
        }
        prev_hit = hit;
        prev = c;
    }
    if q.peek().is_some() {
        return None;
    }
    Some(total - (last - matched) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keymap, parse_chord};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn pal(q: &str) -> Palette {
        Palette {
            query: q.into(),
            selected: 0,
        }
    }

    #[test]
    fn subsequence_matches() {
        assert!(score("sva", "Save As").is_some());
    }

    #[test]
    fn non_subsequence_none() {
        assert_eq!(score("xyz", "Save As"), None);
        assert_eq!(score("ss", "Save"), None);
    }

    #[test]
    fn word_start_beats_middle() {
        assert!(score("tw", "Toggle Wrap") > score("tw", "Delete Word Back"));
        let km = Keymap::defaults();
        assert_eq!(pal("tw").items(&km)[0].name, "Toggle Wrap");
    }

    #[test]
    fn empty_query_lists_all_palette_commands() {
        let km = Keymap::defaults();
        let items = pal("").items(&km);
        assert_eq!(
            items.len(),
            COMMANDS.iter().filter(|c| c.in_palette).count()
        );
        assert!(items.windows(2).all(|w| w[0].name <= w[1].name));
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn keys_of(km: &Keymap, name: &str) -> String {
        let items = pal("").items(km);
        items.into_iter().find(|i| i.name == name).unwrap().keys
    }

    #[test]
    fn palette_shows_remapped_key() {
        let mut km = Keymap::defaults();
        assert_eq!(keys_of(&km, "Redo"), "Ctrl+Y, Ctrl+Shift+Z");
        km.bind(parse_chord("ctrl+l").unwrap(), CommandId::SaveAs);
        assert_eq!(keys_of(&km, "Save As"), "Ctrl+L");
        assert_eq!(keys_of(&km, "Toggle Wrap"), "");
    }

    #[test]
    fn palette_hides_f1() {
        let km = Keymap::defaults();
        assert!(
            pal("")
                .items(&km)
                .iter()
                .all(|i| i.id != CommandId::Palette)
        );
        assert!(!keys_of(&km, "Quit").contains("F1"));
    }

    #[test]
    fn palette_selection_wraps() {
        let km = Keymap::defaults();
        let n = pal("").items(&km).len();
        let mut p = pal("");
        let none = KeyModifiers::NONE;
        p.handle_key(&key(KeyCode::Up, none), &km);
        assert_eq!(p.selected, n - 1);
        p.handle_key(&key(KeyCode::Down, none), &km);
        assert_eq!(p.selected, 0);
        p.handle_key(&key(KeyCode::Char('n'), KeyModifiers::CONTROL), &km);
        assert_eq!(p.selected, 1);
        p.handle_key(&key(KeyCode::Char('p'), KeyModifiers::CONTROL), &km);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn typing_filters_and_resets_selection() {
        let km = Keymap::defaults();
        let mut p = pal("");
        let none = KeyModifiers::NONE;
        p.handle_key(&key(KeyCode::Down, none), &km);
        for c in "sva".chars() {
            p.handle_key(&key(KeyCode::Char(c), none), &km);
        }
        assert_eq!(p.selected, 0);
        let out = p.handle_key(&key(KeyCode::Enter, none), &km);
        assert_eq!(out, PaletteOutcome::Run(CommandId::SaveAs));
        p.handle_key(&key(KeyCode::Backspace, none), &km);
        assert_eq!(p.query, "sv");
    }

    #[test]
    fn enter_with_no_items_and_esc_and_other_keys() {
        let km = Keymap::defaults();
        let none = KeyModifiers::NONE;
        let mut p = pal("zzzz");
        assert_eq!(
            p.handle_key(&key(KeyCode::Enter, none), &km),
            PaletteOutcome::Pending
        );
        assert_eq!(
            p.handle_key(&key(KeyCode::F(5), none), &km),
            PaletteOutcome::Pending
        );
        let ctrl_s = key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(p.handle_key(&ctrl_s, &km), PaletteOutcome::Pending);
        assert_eq!(p.query, "zzzz");
        assert_eq!(
            p.handle_key(&key(KeyCode::Esc, none), &km),
            PaletteOutcome::Close
        );
    }
}
