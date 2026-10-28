# nib — Specification

nib is a small, fast, distraction-free terminal text editor in the spirit of nano.
Built first for its author's own notes and config editing; published for anyone who wants it.
Reliability beats features: nib must never lose or corrupt a file.

## 1. Scope

**Stack:** Rust · `crossterm` (terminal) · `ropey` (buffer) · `arboard` (clipboard) · TOML config.

**Platforms:** Debian, Ubuntu, Fedora on x86_64 (primary terminal: Ghostty) · Windows 10/11 x64 (Windows Terminal and conhost).

**Out of scope:** plugins/extensions · LSP/autocomplete · split panes · file tree · config scripting ·
regex search · multiple buffers/tabs · block selection · multiple cursors · hard wrap · notes sync
(parked as a separate future tool, not part of the editor).

## 2. Screen

```
 nib 0.1.0                    todo.md                    2026-10-27 14:02
 <text area>
                                                        ┌───────────────┐
                                                        │ Saved todo.md │
                                                        └───────────────┘
 <status line — prompts; blank otherwise>
```

- **Title bar** (nano layout): `nib <version>` left · file name centre · state right.
  Right side shows exactly one of:
  - the file's last-modified time on disk (`YYYY-MM-DD HH:MM`) when the buffer is clean;
  - `Modified` from the first change until the next save;
  - `New File` for a buffer never saved.
  - `[Read-only]` is appended when the file is not writable.
- **Text area:** soft wrap on by default; line numbers off by default. Both toggle via palette and config.
  Line numbers sit in a gutter as wide as the last line number's digits plus one space, drawn
  right-aligned in dark grey on the first row of each line (wrapped continuation rows get a blank
  gutter); the text gets the rest of the width. A click in the gutter puts the cursor at the start
  of that row. Turning wrap on or off keeps the cursor on screen.
- **Status line:** empty by default; prompts appear here.
- **Messages:** every message nib shows (errors and confirmations alike; "says" and "status" in
  this spec mean this) appears in a small box floating at the bottom right of the text area, on
  the three rows just above the status line, its right edge one column in from the screen's. The
  column on each side of the box is blanked on those rows, so a wide character cut by the box
  never leaves half of itself on screen.
  Box-drawing border in bright black, text in the default colour, one space of padding each side.
  It fades 4 s after it appeared; a new message replaces the old one and restarts the 4 s; keys
  do not dismiss it early. A message wider than the screen allows (a 1-column margin each side,
  so `width - 6` cells of text) is cut and ends in `…`. On a screen under 12 columns or 6 rows,
  the message goes on the status line instead. So does a message whose box would cover the text
  cursor, so the cursor and the text being typed are never hidden. A message that falls back while
  a prompt holds the status line is not shown (the prompt wins). The palette is drawn over the box.
  - Announced actions (nothing else, so typing and movement stay quiet):
    Save / Save As `Saved <name>` · Open `Opened <name>`, or `New file <name>` for a file not on
    disk yet (opening at startup says nothing) · Copy / Cut / Paste `Copied <n>`, `Cut <n>`,
    `Pasted <n>`, where `<n>` is `1 line` / `<k> lines` when the text holds a line break (a
    trailing partial line counts), else `1 char` / `<k> chars` (grapheme clusters) · Paste with
    an empty clipboard `Nothing to paste` · Undo / Redo `Undo` / `Redo`, or `Nothing to undo` /
    `Nothing to redo` · Toggle Wrap `Wrap on` / `Wrap off` · Toggle Line Numbers
    `Line numbers on` / `Line numbers off` · Select All `Selected all`.
    Bracketed paste (the terminal's own paste) is typing and is not announced.
- **No shortcut bar.** The command palette replaces it.
- **Colours:** the terminal's 16 ANSI colours only, so nib follows the terminal theme.

## 3. Command line

| Invocation | Result |
|---|---|
| `nib` | Empty buffer. First save asks for a name. |
| `nib name` (new) | Buffer for `name.txt`. |
| `nib name.md` / `nib .rc` / `nib Makefile` (new) | Name kept as typed (has extension, starts with a dot, or is a known extensionless name). |
| `nib path` (existing) | Opens exactly that file; never renamed. |
| `nib path:42` | Opens at line 42. Only a trailing `:<digits>` counts, and only when no file with the full name exists (keeps `C:\x.md` safe). |
| `nib --version` / `-V` | Print `nib X.Y.Z` and exit. |
| `nib --help` / `-h` | Print help and exit: usage, arguments, options, examples, the main keys as currently bound (from the user's config), and the config and recovery paths for this system. |

- The `.txt` rule applies only to new files: no extension **and** not starting with `.` **and** not one of `Makefile`, `Dockerfile`, `LICENSE`, `README` (matched case-insensitively). Only the final path component is checked, so `notes.d/todo` becomes `notes.d/todo.txt`. Same rule for the first-save name prompt.
- New files are created on **first save**, never on launch.
- One file at a time. Opening another file (Ctrl+O) replaces the buffer; unsaved changes prompt
  `Save? (Y)es / (N)o / (C)ancel`. Quit prompts the same way. Yes saves first (asking for a name
  when unnamed); if that save fails or is declined, the current buffer stays.
- **Open** then asks `Open: `. The name (trimmed; empty cancels) resolves like the command line,
  `:N` suffix included. A missing file gives an empty `New File` buffer, created on first save.
  Opening resets undo history, cursor, scroll, indent detection and any find. A file nib cannot
  edit leaves the current buffer untouched and says why:
  `<name> is not a UTF-8 text file`, `<name> is a directory`, `<name> is not a regular file`.
  A file over 50 MB asks first, as at startup; N or Esc keeps the current buffer.
- **Save As** asks `Save as: ` prefilled with the current path (empty when unnamed), with the same
  name rules and `<name> exists. Replace? (y/N)` confirm as the first-save prompt; the buffer's own
  path needs no confirm. On success the buffer takes the new path and keeps its undo history.
  An existing target that is not writable is refused.
- **Go to line** (Ctrl+G) asks `Line: `; 1-based, surrounding spaces ignored, 0 and past-the-end
  clamp to the first/last line. Anything else says `Not a line number` and leaves the cursor.
  The cursor lands at the start of the line with no selection.

## 4. Keys

All bindings are remappable in the config. Defaults:

| Action | Key | Action | Key |
|---|---|---|---|
| Command palette | **Ctrl+P** (hidden backup: F1) | Undo / Redo | Ctrl+Z / Ctrl+Y, Ctrl+Shift+Z |
| Save | Ctrl+S | Copy / Cut / Paste | Ctrl+C / Ctrl+X / Ctrl+V |
| Save As | palette only | Cut line (no selection) | Ctrl+X |
| Open file | Ctrl+O | Select all | Ctrl+A |
| Quit | Ctrl+Q | Select | Shift+arrows, Shift+Home/End, mouse drag |
| Find; next / prev | Ctrl+F; Enter / Shift+Enter | Word move (select) | Ctrl+←/→ (+Shift) |
| Replace | Ctrl+R | File start / end | Ctrl+Home / Ctrl+End |
| Go to line | Ctrl+G | Delete word | Ctrl+Backspace / Ctrl+Delete |
| Toggle wrap / line numbers | palette only | Cancel prompt | Esc |

Rationale: Ctrl+Shift+P is taken by both Ghostty and Windows Terminal; Ctrl+P is free in both
(verified: Ghostty `Config.zig` defaults, Windows Terminal 1.24 `defaults.json`). Replace is Ctrl+R
because Ctrl+H is Backspace in legacy terminals. Save As has no key because Ctrl+Shift+S is
indistinguishable from Ctrl+S in some terminals. XON/XOFF flow control is disabled in raw mode
so Ctrl+S / Ctrl+Q reach nib.

## 5. Editing

- **Selection:** one linear selection, any number of lines, drawn in reverse video (a selected line end shows as one reversed space). Typing, Backspace, Delete and word-delete replace/remove exactly the selection. Plain Left/Right collapse to the selection's start/end. An empty selection counts as none.
- **Mouse:** click places cursor · drag selects · double-click (second press within 400 ms on the same cell) selects the word, punctuation or whitespace run under it · wheel scrolls 3 rows without moving the cursor. Clicks on the title or status row, other buttons and motion are ignored.
- **Find:** Ctrl+F opens `Find: `; plain text, smart case (insensitive unless the query has a capital; a char whose lowercase is several chars, like `İ`, matches only itself). Every edit of the query searches again from where the cursor was when Find opened (forward, wrapping) and selects the match; other matches on screen are underlined, the current one is reverse video. Enter selects the next match, Shift+Enter the previous (some legacy terminals cannot tell Shift+Enter from Enter); both wrap with status `Search wrapped`. No match: status `Not found`, selection cleared, cursor back at the start. Esc closes and restores the cursor and selection. Any other key bound to a command (arrows, Ctrl+S, ...) closes Find, keeping the match selected, and then runs. An empty query highlights nothing.
- **Replace:** Ctrl+R: `Find: ` then `Replace with: `, then for each match from the cursor onward (wrapping, ending where it began): `Replace? (R)eplace / (A)ll / (S)kip / Esc`. R replaces and moves on (one undo step each), S skips, A replaces all remaining matches as one undo step, Esc stops and keeps what was replaced. The search resumes after the inserted text, so a replacement containing the query cannot loop. Status `Replaced N` when any were replaced.
- **Undo/redo:** in memory per session; typing grouped by word/pause.
- **Paste:** bracketed paste; a paste is one undo step and bypasses auto-indent and list continuation. In a bracketed paste, `\r\n` and lone `\r` both become newlines (some terminals send `\r`).
- **Auto-indent:** Enter copies the current line's leading whitespace, up to the cursor column (Enter inside the indentation does not duplicate it). With a selection, Enter replaces it and uses the line where the selection starts. Enter is one undo step.
- **List continuation:** Enter on `- x`, `* x`, `1. x`, `- [ ] x` starts the next item (numbers increment, checkboxes reset to `[ ]`); Markers are recognised after the indent; `* [ ] x` counts too, and a checkbox `[x]`/`[X]` resets to `[ ]`. Numbers drop leading zeros (`09.` becomes `10.`). Continuation applies only when the cursor is at or after the end of the marker; Enter inside the marker is a plain Enter. Enter on an empty item (only indent and marker) clears the line and leaves the cursor at column 0 without adding a line (one undo step).
- **Tab:** inserts spaces up to the next multiple of the indent width, measured in display columns (so tabs and wide characters count correctly). Width is detected once at load from the first 10,000 lines (most common step, 2 to 8, between consecutive non-blank lines (an unindented line counts as 0; a tab-indented line resets it)), else 4 (2 for `.json`). Files with more tab-indented than space-indented lines, any `Makefile` (any case) and `*.mk` files use a real tab. With a selection, Tab replaces it with one indent unit.
- **Syntax highlighting:** small hand-written highlighters for Markdown and JSON only, picked by extension in any case: `.md`/`.markdown` is Markdown, `.json` is JSON, anything else is plain.
  - Markdown: headings (1 to 6 `#` then a space or the line end, after up to 3 spaces; the whole line); emphasis `*x*`, `**x**`, `_x_`, `__x__` (the opener touches text on its right, the closer on its left; `_` inside a word, as in `snake_case`, is not emphasis); inline code (closed by a backtick run of the same length); fenced code blocks (a line starting with three backticks after up to 3 spaces opens or closes a block; fence lines and everything between are code); links `[text](target)`; list markers `- `, `* `, `1. ` after any indent (the marker, not its space). Inside code nothing else is recognised.
  - JSON: keys (strings followed by `:`), other strings (an unclosed string runs to the line end), numbers, `true`/`false`/`null`, and the punctuation `{}[]:,`.
  - Colours: headings and literals magenta, emphasis and numbers yellow, code and strings green, links and keys blue, list markers cyan, punctuation bright black. Selected cells drop their colour (plain reverse video); underlined find matches keep it.
  - Only the first 10,000 characters of a line are highlighted, so very long lines stay fast. Fence state for lines above the screen is cached per line and rescanned only from the first edited line.
- **Command palette:** lists every command (all but the palette itself, movement, selection and editing included) with its current key; fuzzy search; driven by the same command registry as the keymap. A command run from the palette behaves exactly like its key, once the palette has closed.
  - Box at the top centre under the title bar (60 columns at most, 10 rows at most), a `> query` line first; the selected row is reversed; each row shows the name and its current keys, right-aligned. Under 20 columns or 5 rows the border is dropped. Fuzzy search is a case-insensitive subsequence: +10 per consecutive character, +8 at a word start, -1 per skipped character; ties sort by name. Typing and Backspace edit the query; Up/Down (and Ctrl+P/Ctrl+N) move the selection with wrap-around; Enter runs the command (after closing the palette); Esc closes. Mouse and paste are ignored while it is open.
  - Find Next / Find Previous (palette only) repeat the last find query from the cursor, wrapping; they say `Not found`, or `No previous search` when there is none.
- **Clipboard:** system clipboard when a display exists (`DISPLAY`/`WAYLAND_DISPLAY` set, or Windows); otherwise OSC 52 (SSH, headless, TTY). Copy with no selection does nothing and says `Nothing selected`. OSC 52 payloads over 100 KB of base64 are not sent (status `Copied (too large for terminal clipboard)`); the copy stays inside nib.
- **Rendering:** draw only the visible region; stays fast on very long lines and terminal resizes.
- **Unicode:** cursor and wrapping respect grapheme clusters and display width (CJK, emoji).

## 6. Files and safety

- **Encoding:** UTF-8 only; a UTF-8 BOM is preserved.
- **Line endings:** each file keeps its own (LF/CRLF); new files use LF on every OS (configurable).
- **Final newline:** preserved if present; new files get one.
- **Atomic save:** write temp file in the same directory → `fsync` → rename over the target.
  Symlinks are followed (the real file is replaced, the link stays); mode and owner are preserved.
- **Recovery file:** written every few seconds while dirty; offered on next open after a crash.
  It records the owning PID; a second nib on the same file offers read-only, or recovery if that PID is gone.
  - Location: `%LOCALAPPDATA%\nib\recovery\` (Windows), `~/.local/share/nib/recovery/` (Linux), created when first needed. A named file uses `<hash>.rec`, where the hash is FNV-1a 64 of the absolute path's bytes as 16 lowercase hex digits (not canonicalised: new files do not exist yet). An unnamed buffer uses `untitled-<pid>.rec`, or `untitled-<pid>-<n>.rec` (first free n) when a crashed nib with the same pid left one.
  - Format: `nib-recovery 1`, `pid <n>`, `path <path>`, an empty line, then the exact bytes a save would write. A file that does not parse (or whose text is not UTF-8) counts as no recovery and is never deleted. nib only ever writes over or deletes a recovery or lock file that it wrote itself (header pid is its own); if a named file's `.rec` already exists and is not its own (e.g. Save As onto a path with older unsaved changes), no recovery is written for that buffer and the status says `Recovery file not written: <path> holds older unsaved changes` once.
  - Written through the atomic save path while dirty, at most every 3 s and only when the text changed since the last write. A failed write says `Recovery file not written: <error>` once (again only after a write has worked) and editing goes on.
  - Deleted after a successful save, on quit, when undo returns to the saved state, and when Open replaces the buffer (after it was saved or the user chose not to). Kept after a failed save. If nib is about to exit on a terminal error with a dirty buffer, it writes the file first.
  - Lock: while a named file is open, nib keeps `<hash>.lock` (same header, no text) in the recovery directory, created only if absent (a lock of a nib that is gone, of this nib, or one that does not parse is replaced; a running nib's lock is never touched, and finding one makes the buffer a read-only copy); it goes on quit and on Open-away. A `.lock` or `.rec` whose PID is running and is not this nib's means the file is open elsewhere. PID check: `/proc/<pid>` on Linux, `tasklist` on Windows; other systems count every PID as gone.
  - On open (startup and Open): if another nib has the file, `already open in another nib (pid N) — open read-only? (Y/n)`; Enter or Y opens it read-only (`[Read-only]`, Save refused, Save As works); this nib takes no lock and writes no recovery for it, but does for a Save As target. N or Esc cancels the open: at startup nib exits (code 0, printing the reason to stderr if there was one), on Open the current buffer stays.
  - Otherwise, if a recovery file is left from a nib that is gone: `Recover unsaved changes from YYYY-MM-DD HH:MM? (Y/n)` (the recovery file's time). Y loads its text as a modified buffer for the same path; the file on disk is untouched until a save, and undo cannot reach a state that counts as saved. N moves it to `<hash>.rec.bak` (replacing an older one), opens the file from disk and says `Recovery kept at <path>`. Enter means Y. Esc cancels the open, as above; if the rename fails, the open is cancelled too, with `Recovery file not moved: <error>`.
  - Save As onto a file another running nib has open is refused with `<name> is open in another nib (pid N)`; the buffer stays modified and nothing is written.
  - These two prompts are the only choices where Enter means yes.
  - Unnamed buffers are never offered at startup (there is no path to match); their `untitled-*.rec` files stay on disk after a crash for manual recovery.
- **External change:** detected before save: if the file's modification time differs from when
  it was read or last saved, Save asks `File changed on disk — overwrite? (y/N)`; Y writes, N, Esc
  or Enter say `Not saved` and keep the buffer modified. Save As to another path does not ask.
- **Panic:** a panic hook restores the terminal (raw mode off, main screen back) before exit.

### Error behaviour

| Situation | Behaviour |
|---|---|
| Binary or non-UTF-8 file | Message screen (at startup): `<name> is not a UTF-8 text file`, centred; exit (code 0) on Enter / Space / Esc. File untouched. |
| Path is a directory | Same as above, with `<name> is a directory`. |
| FIFO, device or socket | Same as above, with `<name> is not a regular file`; it is never read. |
| File > 50 MB | `File is 2.1 GB — open anyway? (y/N)` (1 decimal, KB/MB/GB/TB with 1024 base). N or Esc at startup exits (code 0). |
| Already open in another nib | `already open in another nib (pid N) — open read-only? (Y/n)`; see Recovery file above. |
| No write permission | `[Read-only]` in title; editing allowed; Save says `Cannot save: permission denied — use Save As`; Save As works. Permission is checked again on Save, so a fixed file saves. No built-in sudo. |
| Disk full / write error | Original untouched; `Save failed: <error>`; buffer and recovery file kept. |
| File deleted while open | Save recreates it. |
| Broken config | Defaults used; warning shown as a message. |

## 7. Configuration

TOML at `~/.config/nib/config.toml` (Linux) or `%APPDATA%\nib\config.toml` (Windows).
Holds keymap overrides and settings: wrap, line numbers, tab width, new-file line ending.
Under `sudo`, root's config is used. Read once at startup. Every entry is optional; a missing
file means all defaults. Schema (values shown are the defaults):

```toml
wrap = true            # soft wrap
line_numbers = false
tab_width = 4          # 1 to 8; unset by default. When set, replaces the detected indent
                       # width; files that indent with real tabs keep tabs, shown this wide
line_ending = "lf"     # "lf" | "crlf"; for new files only (existing files keep theirs)

[keys]
save = "ctrl+s"                  # one chord
redo = ["ctrl+y", "ctrl+shift+z"]  # or a list
```

- `[keys]` names are each command's config key: `palette`, `save`, `save_as`, `open`, `quit`,
  `find`, `find_next`, `find_prev`, `replace`, `goto_line`, `undo`, `redo`, `copy`, `cut`,
  `paste`, `select_all`, `toggle_wrap`, `toggle_line_numbers`, `left`, `right`, `up`, `down`,
  `word_left`, `word_right`, `home`, `end`, `page_up`, `page_down`, `file_start`, `file_end`,
  `sel_left`, `sel_right`, `sel_up`, `sel_down`, `sel_word_left`, `sel_word_right`, `sel_home`,
  `sel_end`, `backspace`, `delete`, `delete_word_back`, `delete_word_fwd`, `newline`, `tab`.
- Chords are modifiers `ctrl`, `alt`, `shift` joined with `+` to one key (any case): a
  character, `f1`–`f12`, `enter`, `esc`, `tab`, `backspace`, `delete`, `insert`, `home`, `end`,
  `pageup`, `pagedown`, `up`, `down`, `left`, `right`, `space`. An uppercase letter means
  Shift plus that letter.
- An override replaces all of that command's default chords (hidden ones such as F1 included);
  an empty list leaves it palette-only. Each chord it names is removed from whichever command
  had it by default.
- Any error — invalid TOML, an unknown setting, command or key name, a value of the wrong type
  or out of range, or one chord set for two commands — ignores the whole file: nib starts with
  all defaults and shows `Config error: <msg> — using defaults`.

## 8. Distribution

- **Versioning:** semver from 0.1.0. Pushing a tag `vX.Y.Z` builds a GitHub Release.
- **CI:** GitHub Actions on Linux and Windows — `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.
- **Linux builds** are static musl (`x86_64-unknown-linux-musl`) so one binary runs on every supported distro.
- **Release assets:**
  - `nib_X.Y.Z_amd64.deb` (`cargo-deb`)
  - `nib-X.Y.Z.x86_64.rpm` (`cargo-generate-rpm`)
  - `nib-X.Y.Z-x86_64-linux.tar.gz`
  - `nib-X.Y.Z-x86_64.msi` (`cargo-wix`; per-machine into Program Files; sets PATH; upgrades in place; listed in *Add or remove programs*)
  - `nib-X.Y.Z-x86_64-windows.zip` (portable)
  - `SHA256SUMS`
- **Unsigned** in v1. README shows SmartScreen's *More info → Run anyway*.

### Installers (run once, leave nothing behind)

- **`install.sh`** — `curl -fsSL <url>/install.sh | sh`
  - Reads `/etc/os-release`; on Debian/Ubuntu installs the `.deb` via `apt`, on Fedora the `.rpm` via `dnf`. Binary lands in `/usr/bin`.
  - `--user` (or another distro): tarball into `~/.local/bin`; adds an idempotent PATH line to `~/.profile`, `~/.bashrc`, `~/.zshrc`.
  - Verifies SHA-256, runs `nib --version` by full path, prints a `hash -r` / `rehash` hint, deletes its temp files.
  - `--uninstall` removes a `--user` install and its PATH lines; `--purge` also removes config.
  - Re-running updates.
- **`install.ps1`** — `irm <url>/install.ps1 | iex`
  - Downloads and verifies the `.msi`, runs it silently with `msiexec`. Uninstall via *Add or remove programs*.
- **Package repositories** (APT repo, COPR, official archives): not in v1.

## 9. README contents

Pitch + screenshots · feature highlights · quick start · supported systems (Debian, Ubuntu, Fedora x86_64; Windows 10/11 x64) ·
install / update / uninstall per method · SmartScreen note · build from source · usage · key table · palette ·
config reference · license. No limitations section. No AI/tool credits.
