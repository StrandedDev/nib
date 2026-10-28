//! Indent and list rules for Enter and Tab.

use ropey::Rope;

/// Lines examined by `detect_indent`, so huge files stay fast.
const SCAN_LINES: usize = 10_000;

/// The indent unit the Tab key inserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indent {
    Tabs,
    Spaces(u8),
}

pub fn detect_indent(rope: &Rope, file_name: &str) -> Indent {
    let name = file_name.to_ascii_lowercase();
    if name == "makefile" || name.ends_with(".mk") {
        return Indent::Tabs;
    }
    let (mut tabs, mut spaces) = (0usize, 0usize);
    let mut diffs = [0usize; 9];
    let mut prev: Option<usize> = None;
    for line in rope.lines().take(SCAN_LINES) {
        if line.chars().all(char::is_whitespace) {
            continue;
        }
        match line.char(0) {
            '\t' => {
                tabs += 1;
                prev = None;
            }
            ' ' => {
                spaces += 1;
                let n = line.chars().take_while(|&c| c == ' ').count();
                if let Some(d @ 2..=8) = prev.and_then(|p| n.checked_sub(p)) {
                    diffs[d] += 1;
                }
                prev = Some(n);
            }
            _ => prev = Some(0),
        }
    }
    if tabs > spaces {
        return Indent::Tabs;
    }
    let mut best = 0;
    for d in 2..=8 {
        if diffs[d] > diffs[best] {
            best = d;
        }
    }
    if best > 0 {
        return Indent::Spaces(best as u8);
    }
    Indent::Spaces(if name.ends_with(".json") { 2 } else { 4 })
}

/// What Enter does on a line, given the cursor column.
#[derive(Debug, PartialEq, Eq)]
pub enum Enter {
    /// Insert this text (starting with `\n`) at the cursor.
    Insert(String),
    /// An empty list item: blank the line instead of adding one.
    ClearLine,
}

/// `line` has no trailing newline; `col` is a char index into it.
pub fn plan_enter(line: &str, col: usize, has_selection: bool) -> Enter {
    let indent: String = line
        .chars()
        .take(col)
        .take_while(|c| matches!(c, ' ' | '\t'))
        .collect();
    if let Some(m) = list_marker(line) {
        if col >= m.len {
            if m.empty && !has_selection {
                return Enter::ClearLine;
            }
            return Enter::Insert(format!("\n{}", m.next));
        }
    }
    Enter::Insert(format!("\n{indent}"))
}

struct Marker {
    /// Chars from line start through the marker's trailing space.
    len: usize,
    /// Indent plus the marker for the next item.
    next: String,
    /// Nothing but whitespace follows the marker.
    empty: bool,
}

fn list_marker(line: &str) -> Option<Marker> {
    let indent_len = line.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
    let indent: String = line.chars().take(indent_len).collect();
    let rest: String = line.chars().skip(indent_len).collect();
    let bullet = ["- ", "* "].into_iter().find(|b| rest.starts_with(b));
    let (marker, tail) = if let Some(b) = bullet {
        let t = &rest[b.len()..];
        match ["[ ] ", "[x] ", "[X] "]
            .into_iter()
            .find(|c| t.starts_with(c))
        {
            Some(c) => (format!("{b}[ ] "), &t[c.len()..]),
            None => (b.to_string(), t),
        }
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let tail = rest[digits..].strip_prefix(". ")?;
        let n: u64 = rest[..digits].parse().ok()?;
        (format!("{}. ", n.checked_add(1)?), tail)
    };
    let len = line.chars().count() - tail.chars().count();
    Some(Marker {
        len,
        next: format!("{indent}{marker}"),
        empty: tail.trim().is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn detect(s: &str, name: &str) -> Indent {
        detect_indent(&Rope::from_str(s), name)
    }

    #[test]
    fn detect_two_space() {
        let src = "a:\n  b:\n    c: 1\n  d: 2\n";
        assert_eq!(detect(src, "x.yaml"), Indent::Spaces(2));
    }

    #[test]
    fn detect_depth_one_two_space() {
        assert_eq!(detect("a:\n  b\nc:\n  d", "x.yaml"), Indent::Spaces(2));
    }

    #[test]
    fn detect_depth_one_four_space_python_like() {
        let src = "def f():\n    x\n\ndef g():\n    y";
        assert_eq!(detect(src, "x.py"), Indent::Spaces(4));
        // Distinguishes from the fallback: 3 would otherwise be 4.
        assert_eq!(detect("a\n   b\nc\n   d", "x.txt"), Indent::Spaces(3));
    }

    #[test]
    fn blank_line_does_not_reset() {
        assert_eq!(detect("a:\n\n  b", "x.yaml"), Indent::Spaces(2));
    }

    #[test]
    fn detect_tabs() {
        assert_eq!(detect("a\n\tb\n\t\tc\n  d\n", "x.rs"), Indent::Tabs);
    }

    #[test]
    fn makefile_tabs() {
        assert_eq!(detect("all:\n    gcc\n", "Makefile"), Indent::Tabs);
    }

    #[test]
    fn makefile_uppercase_and_mk() {
        assert_eq!(detect("", "MAKEFILE"), Indent::Tabs);
        assert_eq!(detect("a\n  b\n", "rules.MK"), Indent::Tabs);
        assert_eq!(detect("", "build.mk"), Indent::Tabs);
    }

    #[test]
    fn json_default_two() {
        assert_eq!(detect("{}", "a.json"), Indent::Spaces(2));
        assert_eq!(detect("{}", "a.txt"), Indent::Spaces(4));
        assert_eq!(detect("", ""), Indent::Spaces(4));
    }

    #[test]
    fn detect_indent_caps_scan() {
        let mut s = String::new();
        for _ in 0..5_000 {
            s.push_str("  a\n    b\n");
        }
        for _ in 0..8_000 {
            s.push_str("        a\n                b\n");
        }
        assert_eq!(detect(&s, "x.txt"), Indent::Spaces(2));
    }
}
