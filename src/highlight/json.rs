//! JSON: keys, strings, numbers, literals and punctuation.

use std::ops::Range;

use super::Style;

pub(super) fn line(line: &str) -> Vec<(Range<usize>, Style)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let (end, style) = match b[i] {
            b'{' | b'}' | b'[' | b']' | b':' | b',' => (i + 1, Style::Punct),
            b'"' => {
                let end = string_end(b, i);
                (end, key_or_string(b, end))
            }
            b'-' | b'0'..=b'9' => (run_end(b, i, is_number_byte), Style::Number),
            c if c.is_ascii_alphabetic() => {
                let end = run_end(b, i, |c| c.is_ascii_alphanumeric());
                let lit = matches!(&b[i..end], b"true" | b"false" | b"null");
                (end, if lit { Style::Literal } else { Style::Normal })
            }
            _ => (i + 1, Style::Normal),
        };
        if style != Style::Normal {
            out.push((i..end, style));
        }
        i = end;
    }
    out
}

fn is_number_byte(c: u8) -> bool {
    matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
}

/// End of the run of bytes from `i` that satisfy `f`.
fn run_end(b: &[u8], i: usize, f: impl Fn(u8) -> bool) -> usize {
    i + b[i..].iter().take_while(|&&c| f(c)).count()
}

/// End of the string opening at `i`, past its closing quote, or the line
/// end when it is not closed.
fn string_end(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// A string followed by `:` is a key.
fn key_or_string(b: &[u8], end: usize) -> Style {
    let next = b[end..].iter().find(|c| !c.is_ascii_whitespace());
    if next == Some(&b':') {
        Style::Key
    } else {
        Style::String
    }
}
