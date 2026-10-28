//! Single-line prompts shown on the status line.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

pub enum Prompt {
    Text {
        label: String,
        input: String,
        cursor: usize,
    },
    Choice {
        label: String,
        keys: &'static [char],
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum PromptResult {
    Pending,
    Submit(String),
    Choice(char),
    Cancel,
}

impl Prompt {
    pub fn handle_key(&mut self, key: &KeyEvent) -> PromptResult {
        if key.kind == KeyEventKind::Release {
            return PromptResult::Pending;
        }
        if key.code == KeyCode::Esc {
            return PromptResult::Cancel;
        }
        match self {
            Prompt::Choice { keys, .. } => match key.code {
                // Ctrl+A and the like are shortcuts, not answers.
                KeyCode::Char(_)
                    if key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    PromptResult::Pending
                }
                KeyCode::Char(c) => {
                    let c = c.to_ascii_lowercase();
                    if keys.contains(&c) {
                        PromptResult::Choice(c)
                    } else {
                        PromptResult::Pending
                    }
                }
                // Enter takes the safe default, same as Esc.
                KeyCode::Enter => PromptResult::Cancel,
                _ => PromptResult::Pending,
            },
            Prompt::Text { input, cursor, .. } => edit_text(input, cursor, key),
        }
    }

    /// An empty text prompt.
    pub fn text(label: &str) -> Prompt {
        Prompt::Text {
            label: label.to_string(),
            input: String::new(),
            cursor: 0,
        }
    }

    /// Inserts pasted text into a text prompt; control chars (newlines) are
    /// dropped. Choice prompts ignore pastes.
    pub fn paste(&mut self, s: &str) {
        if let Prompt::Text { input, cursor, .. } = self {
            for c in s.chars().filter(|c| !c.is_control()) {
                input.insert(*cursor, c);
                *cursor += c.len_utf8();
            }
        }
    }
}

fn prev_boundary(s: &str, at: usize) -> usize {
    s[..at]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

fn next_boundary(s: &str, at: usize) -> usize {
    s[at..].graphemes(true).next().map_or(at, |g| at + g.len())
}

/// `cursor` is a byte offset into `input`, always on a grapheme boundary.
fn edit_text(input: &mut String, cursor: &mut usize, key: &KeyEvent) -> PromptResult {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Enter => return PromptResult::Submit(input.clone()),
        KeyCode::Left => *cursor = prev_boundary(input, *cursor),
        KeyCode::Right => *cursor = next_boundary(input, *cursor),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = input.len(),
        KeyCode::Backspace => {
            let start = prev_boundary(input, *cursor);
            input.replace_range(start..*cursor, "");
            *cursor = start;
        }
        KeyCode::Delete => {
            let end = next_boundary(input, *cursor);
            input.replace_range(*cursor..end, "");
        }
        // AltGr arrives as Ctrl+Alt on Windows.
        KeyCode::Char(c) if !c.is_control() && (ctrl == alt) => {
            input.insert(*cursor, c);
            *cursor += c.len_utf8();
        }
        _ => {}
    }
    PromptResult::Pending
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn choice_accepts_listed_keys_any_case() {
        let mut p = Prompt::Choice {
            label: "Save?".into(),
            keys: &['y', 'n', 'c'],
        };
        assert_eq!(
            p.handle_key(&key(KeyCode::Char('Y'))),
            PromptResult::Choice('y')
        );
        assert_eq!(
            p.handle_key(&key(KeyCode::Char('n'))),
            PromptResult::Choice('n')
        );
        assert_eq!(
            p.handle_key(&key(KeyCode::Char('x'))),
            PromptResult::Pending
        );
        assert_eq!(p.handle_key(&key(KeyCode::Esc)), PromptResult::Cancel);
    }

    #[test]
    fn text_edits_and_submits() {
        let mut p = Prompt::text("File name: ");
        for c in "abd".chars() {
            assert_eq!(p.handle_key(&key(KeyCode::Char(c))), PromptResult::Pending);
        }
        p.handle_key(&key(KeyCode::Left));
        p.handle_key(&key(KeyCode::Char('c')));
        p.handle_key(&key(KeyCode::End));
        p.handle_key(&key(KeyCode::Char('\u{e9}')));
        p.handle_key(&key(KeyCode::Backspace));
        p.handle_key(&key(KeyCode::Home));
        p.handle_key(&key(KeyCode::Delete));
        let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        p.handle_key(&ctrl_a);
        assert_eq!(
            p.handle_key(&key(KeyCode::Enter)),
            PromptResult::Submit("bcd".into())
        );
    }

    #[test]
    fn paste_into_text_drops_control_chars() {
        let mut p = Prompt::text("File name: ");
        p.handle_key(&key(KeyCode::Char('a')));
        p.paste("b\r\nc\x1b");
        p.handle_key(&key(KeyCode::Char('d')));
        assert_eq!(
            p.handle_key(&key(KeyCode::Enter)),
            PromptResult::Submit("abcd".into())
        );
    }

    #[test]
    fn choice_enter_cancels() {
        let mut p = Prompt::Choice {
            label: "Replace?".into(),
            keys: &['y', 'n'],
        };
        assert_eq!(p.handle_key(&key(KeyCode::Enter)), PromptResult::Cancel);
    }

    #[test]
    fn choice_ignores_ctrl_letter() {
        let mut p = Prompt::Choice {
            label: "Replace?".into(),
            keys: &['a', 'r'],
        };
        let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let alt_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT);
        assert_eq!(p.handle_key(&ctrl_a), PromptResult::Pending);
        assert_eq!(p.handle_key(&alt_r), PromptResult::Pending);
        let shift_a = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(p.handle_key(&shift_a), PromptResult::Choice('a'));
    }
}
