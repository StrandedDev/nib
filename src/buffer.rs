use ropey::Rope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMeta {
    pub bom: bool,
    pub line_ending: LineEnding,
    pub final_newline: bool,
}

pub struct Buffer {
    pub rope: Rope,
    pub meta: TextMeta,
    pub dirty: bool,
    /// Bumped on every real text change; keys layout caches.
    pub version: u64,
    /// Lowest line changed since `take_changed_from`; a new buffer counts
    /// as changed from line 0.
    changed_from: usize,
}

const BOM: &str = "\u{FEFF}";

fn detect_line_ending(s: &str) -> LineEnding {
    let crlf = s.matches("\r\n").count();
    let lf = s.matches('\n').count() - crlf;
    if crlf > lf {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

impl Buffer {
    pub fn from_str_detect(s: &str) -> Buffer {
        let (bom, s) = match s.strip_prefix(BOM) {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let line_ending = detect_line_ending(s);
        let mut text = s.replace("\r\n", "\n");
        let final_newline = text.ends_with('\n');
        if final_newline {
            text.pop();
        }
        Buffer {
            rope: Rope::from_str(&text),
            meta: TextMeta {
                bom,
                line_ending,
                final_newline,
            },
            dirty: false,
            version: 0,
            changed_from: 0,
        }
    }

    pub fn new_empty(le: LineEnding) -> Buffer {
        Buffer {
            rope: Rope::new(),
            meta: TextMeta {
                bom: false,
                line_ending: le,
                final_newline: true,
            },
            dirty: false,
            version: 0,
            changed_from: 0,
        }
    }

    /// Records a change starting at char `at`, before the rope changes.
    pub fn note_change(&mut self, at: usize) {
        self.changed_from = self.changed_from.min(self.rope.char_to_line(at));
    }

    /// The lowest line changed since the last call (`usize::MAX` when
    /// none), and resets it.
    pub fn take_changed_from(&mut self) -> usize {
        std::mem::replace(&mut self.changed_from, usize::MAX)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut text = self.rope.to_string();
        if self.meta.final_newline {
            text.push('\n');
        }
        if self.meta.line_ending == LineEnding::CrLf {
            text = text.replace('\n', "\r\n");
        }
        let mut out = Vec::with_capacity(text.len() + BOM.len());
        if self.meta.bom {
            out.extend_from_slice(BOM.as_bytes());
        }
        out.extend_from_slice(text.as_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(s: &str) -> Vec<u8> {
        Buffer::from_str_detect(s).to_bytes()
    }

    #[test]
    fn roundtrip_lf() {
        let b = Buffer::from_str_detect("a\nb\n");
        assert_eq!(b.meta.line_ending, LineEnding::Lf);
        assert!(b.meta.final_newline);
        assert!(!b.meta.bom);
        assert_eq!(b.to_bytes(), b"a\nb\n");
    }

    #[test]
    fn roundtrip_crlf() {
        let b = Buffer::from_str_detect("a\r\nb\r\n");
        assert_eq!(b.meta.line_ending, LineEnding::CrLf);
        assert_eq!(b.rope.to_string(), "a\nb");
        assert_eq!(b.to_bytes(), b"a\r\nb\r\n");
    }

    #[test]
    fn roundtrip_bom() {
        let s = "\u{FEFF}a\r\nb\r\n";
        let b = Buffer::from_str_detect(s);
        assert!(b.meta.bom);
        assert!(!b.rope.to_string().contains('\u{FEFF}'));
        assert_eq!(b.to_bytes(), s.as_bytes());
    }

    #[test]
    fn no_final_newline_preserved() {
        let b = Buffer::from_str_detect("a\nb");
        assert!(!b.meta.final_newline);
        assert_eq!(b.to_bytes(), b"a\nb");
    }

    #[test]
    fn empty_file_roundtrip() {
        let b = Buffer::from_str_detect("");
        assert!(!b.meta.final_newline);
        assert_eq!(b.to_bytes(), b"");
    }

    #[test]
    fn new_empty_gets_newline() {
        let b = Buffer::new_empty(LineEnding::Lf);
        assert!(b.meta.final_newline);
        assert!(!b.dirty);
        assert_eq!(b.to_bytes(), b"\n");
        assert_eq!(Buffer::new_empty(LineEnding::CrLf).to_bytes(), b"\r\n");
    }

    #[test]
    fn lone_newline_roundtrip() {
        assert_eq!(roundtrip("\n"), b"\n");
    }

    #[test]
    fn mixed_endings_use_majority_and_roundtrip() {
        let b = Buffer::from_str_detect("a\r\nb\r\nc\nd\r\n");
        assert_eq!(b.meta.line_ending, LineEnding::CrLf);
        let out = b.to_bytes();
        assert_eq!(out, b"a\r\nb\r\nc\r\nd\r\n");
        assert!(!out.windows(2).any(|w| w == b"\r\r"));
    }

    #[test]
    fn mixed_endings_lf_majority() {
        let b = Buffer::from_str_detect("a\nb\nc\r\nd\n");
        assert_eq!(b.meta.line_ending, LineEnding::Lf);
        assert_eq!(b.to_bytes(), b"a\nb\nc\nd\n");
    }

    #[test]
    fn mixed_endings_tie_goes_to_lf() {
        let b = Buffer::from_str_detect("a\r\nb\n");
        assert_eq!(b.meta.line_ending, LineEnding::Lf);
        assert_eq!(b.to_bytes(), b"a\nb\n");
    }

    #[test]
    fn bom_only_roundtrip() {
        assert_eq!(roundtrip("\u{FEFF}"), "\u{FEFF}".as_bytes());
    }

    #[test]
    fn crlf_without_final_newline_roundtrip() {
        assert_eq!(roundtrip("a\r\nb"), b"a\r\nb");
    }

    #[test]
    fn lone_cr_in_lf_file_roundtrip() {
        assert_eq!(roundtrip("a\rb"), b"a\rb");
    }

    #[test]
    fn lone_cr_is_not_a_line_break() {
        let b = Buffer::from_str_detect("a\rb\n");
        assert_eq!(b.rope.to_string(), "a\rb");
        assert_eq!(b.rope.len_lines(), 1);
    }

    #[test]
    fn cr_before_crlf_roundtrips() {
        let b = Buffer::from_str_detect("a\r\r\nb\r\n");
        assert_eq!(b.meta.line_ending, LineEnding::CrLf);
        assert_eq!(b.to_bytes(), b"a\r\r\nb\r\n");
    }
}
