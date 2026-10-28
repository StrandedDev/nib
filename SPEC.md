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


 <status line — blank unless there is a message>
```

- **Title bar** (nano layout): `nib <version>` left · file name centre · state right.
  Right side shows exactly one of:
  - the file's last-modified time on disk (`YYYY-MM-DD HH:MM`) when the buffer is clean;
  - `Modified` from the first change until the next save;
  - `New File` for a buffer never saved.
  - `[Read-only]` is appended when the file is not writable.
- **Text area:** soft wrap on by default; line numbers off by default. Both toggle via palette and config.
- **Status line:** empty by default; transient messages fade after a few seconds; prompts appear here.
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
| `nib --version`, `nib --help` | Print and exit. |

- The `.txt` rule applies only to new files: no extension **and** not starting with `.` **and** not one of `Makefile`, `Dockerfile`, `LICENSE`, `README` (matched case-insensitively). Only the final path component is checked, so `notes.d/todo` becomes `notes.d/todo.txt`. Same rule for the first-save name prompt.
- New files are created on **first save**, never on launch.
- One file at a time. Opening another file (Ctrl+O) replaces the buffer; unsaved changes prompt
  `Save? (Y)es / (N)o / (C)ancel`. Quit prompts the same way.

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
- **Find:** incremental (highlights as you type), smart case (insensitive unless the query has a capital), plain text.
- **Replace:** per match — this one / all / skip.
- **Undo/redo:** in memory per session; typing grouped by word/pause.
- **Paste:** bracketed paste; a paste is one undo step and bypasses auto-indent and list continuation. In a bracketed paste, `\r\n` and lone `\r` both become newlines (some terminals send `\r`).
- **Auto-indent:** Enter copies the current line's leading whitespace, up to the cursor column (Enter inside the indentation does not duplicate it). With a selection, Enter replaces it and uses the line where the selection starts. Enter is one undo step.
- **List continuation:** Enter on `- x`, `* x`, `1. x`, `- [ ] x` starts the next item (numbers increment, checkboxes reset to `[ ]`); Markers are recognised after the indent; `* [ ] x` counts too, and a checkbox `[x]`/`[X]` resets to `[ ]`. Numbers drop leading zeros (`09.` becomes `10.`). Continuation applies only when the cursor is at or after the end of the marker; Enter inside the marker is a plain Enter. Enter on an empty item (only indent and marker) clears the line and leaves the cursor at column 0 without adding a line (one undo step).
- **Tab:** inserts spaces up to the next multiple of the indent width, measured in display columns (so tabs and wide characters count correctly). Width is detected once at load from the first 10,000 lines (most common step, 2 to 8, between consecutive non-blank lines (an unindented line counts as 0; a tab-indented line resets it)), else 4 (2 for `.json`). Files with more tab-indented than space-indented lines, any `Makefile` (any case) and `*.mk` files use a real tab. With a selection, Tab replaces it with one indent unit.
- **Syntax highlighting:** small hand-written highlighters for Markdown and JSON only.
- **Command palette:** lists every action with its current key; fuzzy search; driven by the same command registry as the keymap.
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
- **External change:** detected before save; user is warned.
- **Panic:** a panic hook restores the terminal (raw mode off, main screen back) before exit.

### Error behaviour

| Situation | Behaviour |
|---|---|
| Binary or non-UTF-8 file | Message screen; exit on Enter / Space / Esc. File untouched. |
| Path is a directory | Same as above. |
| File > 50 MB | `File is 2.1 GB — open anyway? (y/N)` |
| Already open in another nib | `already open in another nib (pid N) — open read-only? (Y/n)` |
| No write permission | `[Read-only]` in title; editing allowed; save fails with message; Save As works. No built-in sudo. |
| Disk full / write error | Original untouched; error shown; buffer and recovery file kept. |
| File deleted while open | Save recreates it. |
| Broken config | Defaults used; warning in status line. |

## 7. Configuration

TOML at `~/.config/nib/config.toml` (Linux) or `%APPDATA%\nib\config.toml` (Windows).
Holds keymap overrides and settings: wrap, line numbers, tab width, new-file line ending.
Under `sudo`, root's config is used.

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

Pitch + screenshot · supported systems (Debian, Ubuntu, Fedora x86_64; Windows 10/11 x64) ·
install / update / uninstall per method · SmartScreen note · usage · key table · palette ·
config reference · license. No limitations section. No AI/tool credits.
