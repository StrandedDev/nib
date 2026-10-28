//! Markdown: headings, emphasis, inline code and fences, links, list markers.

use std::ops::Range;

use super::{LineState, Style};

pub(super) fn line(line: &str, state: &mut LineState) -> Vec<(Range<usize>, Style)> {
    let whole = || (!line.is_empty()).then_some((0..line.len(), Style::Code));
    if is_fence(line) {
        state.in_fence = !state.in_fence;
        return whole().into_iter().collect();
    }
    if state.in_fence {
        return whole().into_iter().collect();
    }
    let (indent, rest) = strip_indent(line);
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && matches!(rest.as_bytes().get(hashes), None | Some(b' ' | b'\t'))
    {
        return vec![(indent..line.len(), Style::Heading)];
    }
    match list_marker(line) {
        Some(m) => {
            let mut out = vec![(m.clone(), Style::ListMark)];
            out.extend(inline(line, m.end));
            out
        }
        None => inline(line, 0),
    }
}

/// The marker of a list item, as list continuation knows them: `- `, `* `
/// or `1. ` after any indent. The range leaves out the trailing space.
fn list_marker(line: &str) -> Option<Range<usize>> {
    let b = line.as_bytes();
    let start = b.iter().take_while(|&&c| c == b' ' || c == b'\t').count();
    let digits = b[start..].iter().take_while(|c| c.is_ascii_digit()).count();
    let end = match b.get(start) {
        Some(b'-' | b'*') => start + 1,
        Some(_) if digits > 0 && b.get(start + digits) == Some(&b'.') => start + digits + 1,
        _ => return None,
    };
    (b.get(end) == Some(&b' ')).then_some(start..end)
}

/// Inline spans of `line` from byte `from`. Every delimiter is ASCII, so
/// byte offsets found here are always char boundaries.
fn inline(line: &str, from: usize) -> Vec<(Range<usize>, Style)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    // Once a closer search fails from some point it fails from every later
    // point too, so pathological lines stay linear-ish.
    let mut no_ticks: Vec<usize> = Vec::new();
    let mut no_emph = [false; 4];
    let mut no_link = false;
    let mut i = from;
    while i < b.len() {
        let found = match b[i] {
            b'`' => code_span(line, i, &mut no_ticks),
            b'*' | b'_' => emphasis(line, i, &mut no_emph),
            b'[' if !no_link => link(line, i, &mut no_link),
            _ => None,
        };
        match found {
            Some((end, style)) => {
                out.push((i..end, style));
                i = end;
            }
            None => i += run_len(b, i),
        }
    }
    out
}

/// Length of the run of `b[i]` starting at `i` (1 for most bytes).
fn run_len(b: &[u8], i: usize) -> usize {
    let c = b[i];
    if matches!(c, b'`' | b'*' | b'_') {
        b[i..].iter().take_while(|&&x| x == c).count()
    } else {
        1
    }
}

/// Emphasis opened by `*`, `**`, `_` or `__` at `i`, closed by the same
/// delimiter. The opener must touch text on its right and the closer on its
/// left; `_` must also not sit inside a word (`snake_case`).
fn emphasis(line: &str, i: usize, failed: &mut [bool; 4]) -> Option<(usize, Style)> {
    let b = line.as_bytes();
    let c = b[i];
    let n = if b.get(i + 1) == Some(&c) { 2 } else { 1 };
    let kind = usize::from(c == b'_') * 2 + n - 1;
    let word = |k: Option<&u8>| k.is_some_and(u8::is_ascii_alphanumeric);
    let opens = b.get(i + n).is_some_and(|x| !x.is_ascii_whitespace())
        && !(c == b'_' && word(i.checked_sub(1).map(|p| &b[p])));
    if failed[kind] || !opens {
        return None;
    }
    let delim = &line[i..i + n];
    let mut j = i + n;
    while let Some(k) = line[j..].find(delim).map(|k| j + k) {
        let closes =
            k > i + n && !b[k - 1].is_ascii_whitespace() && !(c == b'_' && word(b.get(k + n)));
        if closes {
            return Some((k + n, Style::Emph));
        }
        j = k + 1;
    }
    failed[kind] = true;
    None
}

/// A link `[text](target)` opened at `i`: the first `]` must be followed
/// by `(`, and the link ends at the next `)`.
fn link(line: &str, i: usize, failed: &mut bool) -> Option<(usize, Style)> {
    let Some(close) = line[i..].find(']').map(|k| i + k) else {
        *failed = true;
        return None;
    };
    let rest = line[close + 1..].strip_prefix('(')?;
    let Some(end) = rest.find(')').map(|k| close + 2 + k) else {
        *failed = true;
        return None;
    };
    Some((end + 1, Style::Link))
}

/// A code span opened by the backtick run at `i`: it closes on the next run
/// of exactly the same length.
fn code_span(line: &str, i: usize, no_ticks: &mut Vec<usize>) -> Option<(usize, Style)> {
    let b = line.as_bytes();
    let n = run_len(b, i);
    if no_ticks.contains(&n) {
        return None;
    }
    let mut j = i + n;
    while let Some(k) = line[j..].find('`') {
        let start = j + k;
        let m = run_len(b, start);
        if m == n {
            return Some((start + m, Style::Code));
        }
        j = start + m;
    }
    no_ticks.push(n);
    None
}

/// A line that opens or closes a fenced code block: three backticks after
/// up to three spaces. Needs only the start of the line.
pub(super) fn is_fence(line: &str) -> bool {
    strip_indent(line).1.starts_with("```")
}

/// Up to three leading spaces, which Markdown allows before a block marker.
fn strip_indent(line: &str) -> (usize, &str) {
    let n = line.bytes().take(3).take_while(|&b| b == b' ').count();
    (n, &line[n..])
}
