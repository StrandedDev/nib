# How nib works inside

This guide walks through nib's source, from the moment the binary starts to the moment a save
reaches the disk. It is for anyone who wants to read, change or extend the code.

`SPEC.md` says **what** nib does. This file explains **how** the code does it. When the two
disagree, the spec wins and this file is out of date.

## Contents

1. [The big picture](#1-the-big-picture)
2. [Source map](#2-source-map)
3. [Startup](#3-startup)
4. [The terminal guard](#4-the-terminal-guard)
5. [The event loop](#5-the-event-loop)
6. [The text model](#6-the-text-model)
7. [Cursor and graphemes](#7-cursor-and-graphemes)
8. [Editing and undo](#8-editing-and-undo)
9. [Commands, keymap and palette](#9-commands-keymap-and-palette)
10. [Layout: from lines to screen rows](#10-layout-from-lines-to-screen-rows)
11. [Rendering a frame](#11-rendering-a-frame)
12. [Syntax highlighting](#12-syntax-highlighting)
13. [Find and replace](#13-find-and-replace)
14. [Prompts and file flows](#14-prompts-and-file-flows)
15. [Loading files](#15-loading-files)
16. [Saving: the atomic write](#16-saving-the-atomic-write)
17. [Crash recovery and the file lock](#17-crash-recovery-and-the-file-lock)
18. [Clipboard](#18-clipboard)
19. [Configuration](#19-configuration)
20. [Testing](#20-testing)
21. [Build and release](#21-build-and-release)
22. [Recipe: adding a command](#22-recipe-adding-a-command)

---

## 1. The big picture

nib is one process with one buffer and one thread. Everything hangs off a single `App` value
(`src/app.rs`). A loop waits for a terminal event, changes `App`, and redraws the screen if
anything changed.

```
            ┌──────────── main.rs ────────────┐
 argv ────► │ cli::parse → config::load       │
            │ App::open_configured → app.run  │
            └───────────────┬─────────────────┘
                            ▼
   ┌──────────────────── App::run_loop ─────────────────────┐
   │                                                         │
   │  crossterm event ──► handle_event ──► palette / find /  │
   │   (poll 250 ms)        │               prompt / keymap  │
   │                        ▼                                │
   │                 dispatch(CommandId) ──► Editor (edits,  │
   │                        │                 undo, cursor)  │
   │                        ▼                                │
   │        tick: recovery file every ≤ 3 s while dirty      │
   │                        ▼                                │
   │   highlight::Cache::sync ──► render::draw(AppView)      │
   │                                   │                     │
   └───────────────────────────────────┼─────────────────────┘
                                       ▼
                               terminal (stdout)
```

Three ideas run through the whole codebase:

- **Safety first.** Every disk write goes through one atomic-save function. Every exit path,
  panics included, gives the terminal back in a usable state.
- **One registry of commands.** Keys, the palette, the config file and `--help` all read the
  same table, so they cannot drift apart.
- **Cost follows the screen, not the file.** Layout, highlighting and cursor motion only touch
  the lines they need, so a huge file or a huge line stays fast.

## 2. Source map

All code lives in `src/`. `main.rs` is the binary; everything else is a library crate (`lib.rs`)
so tests can reach it.

| File | Job |
|---|---|
| `main.rs` | Parse arguments, load config, build `App`, run it, print any exit note. |
| `cli.rs` | Argument parsing, `FILE:LINE` resolution, the `.txt` rule, `--help` text. |
| `app.rs` | The `App` struct, the event loop, event routing and command dispatch. |
| `terminal.rs` | `Guard`: raw mode, alternate screen, mouse, bracketed paste; panic hook. |
| `buffer.rs` | `Buffer`: a rope plus the file's format (line ending, BOM, final newline). |
| `cursor.rs` | Pure cursor motion over the rope, one grapheme at a time. |
| `edit.rs` | `Editor`: buffer + cursor + selection + undo; all text changes. |
| `undo.rs` | `UndoStack`: grouped undo / redo and the saved point. |
| `indent.rs` | Indent detection, Enter (keep indent, continue lists) and Tab rules. |
| `commands.rs` | `COMMANDS`, the single registry of user actions. |
| `keymap.rs` | Key chords, parsing (`"ctrl+s"`), display (`Ctrl+S`), the lookup map. |
| `palette.rs` | The command palette: fuzzy scoring and key handling. |
| `prompt.rs` | One-line prompts on the status line (`Find:`, `Save as:`, `(Y/n)`). |
| `find.rs` | Incremental find and per-match replace. |
| `files.rs` | File flows: open, save, save as, quit, and the questions between them. |
| `fileio.rs` | Disk access: load, atomic write, mtime, permission checks, formatting. |
| `recovery.rs` | Recovery files and the one-nib-per-file lock. |
| `layout.rs` | Soft wrap and horizontal scroll: buffer lines ↔ screen rows. |
| `render.rs` | Turns one `AppView` into terminal output. |
| `screens.rs` | Full-screen message when the startup file cannot be edited. |
| `highlight/` | `mod.rs` (styles, cache), `markdown.rs`, `json.rs`. |
| `clipboard.rs` | System clipboard, or OSC 52 through the terminal. |
| `config.rs` | The TOML config: settings and key overrides. |

Dependencies are few on purpose (see `Cargo.toml` for why each one is there): `crossterm`
(terminal), `ropey` (text), `unicode-segmentation` and `unicode-width` (graphemes and cell
widths), `arboard` (system clipboard), `base64` (OSC 52), `toml` + `serde` (config), `dirs`
(config and data folders), `jiff` (local time in the title bar) and `anyhow` (errors).

## 3. Startup

`main.rs` is short. In order:

1. **Parse arguments** with `cli::parse`. It only classifies: `--help` / `-h`, `--version` /
   `-V`, `--` (everything after is a file name), one optional path. Unknown flags and a second
   path are errors (exit code 2).
2. **`--help`** loads the config so the key list shows the user's real bindings, then prints
   `cli::help_text(keymap, config path, recovery dir)` and exits.
3. **Resolve the path** with `cli::resolve`. If the exact name exists, it wins. Otherwise a
   trailing `:<digits>` becomes a line number (this keeps `C:\x.md` safe, because `\x.md` is not
   digits). A new name without an extension gets `.txt` (`new_file_name`).
4. **Install the panic hook** (`terminal::install_panic_hook`) so a panic restores the terminal
   before the message prints.
5. **Load the config** (`config::load`). A broken config gives defaults plus a warning, which is
   shown in the status line once nib is running.
6. **Build the app**: `App::open_configured(path, line, Recovery::system(), config)`. This loads
   the file, checks for another nib or a leftover recovery file, detects the indent and moves the
   cursor to the line. It never writes to disk.
7. **Attach the clipboard** (`clipboard::detect`) and **run**.
8. After `run` returns the terminal is already restored, so `main` can safely print the exit
   note (for example why nib refused to open a file) to stderr.

## 4. The terminal guard

`terminal::Guard` owns everything nib changes about the terminal:

- raw mode (keys arrive one by one, no echo; on Unix it also clears XON/XOFF so Ctrl+S and
  Ctrl+Q reach nib instead of freezing output),
- the alternate screen (your shell's scrollback comes back untouched when nib exits),
- mouse capture,
- bracketed paste (a pasted block arrives as one `Event::Paste`, not as typed keys).

`Guard::enter` builds the guard **before** enabling the extras, so if one of them fails the
guard's `Drop` still undoes what was done. `restore()` ignores "not enabled" errors and is safe to
call twice: once from `Drop` and once from the panic hook.

`Cargo.toml` sets `panic = "abort"` in release builds. The panic hook still runs first, so the
terminal is restored even then.

## 5. The event loop

`App::run_loop` is the only place that takes over the terminal:

```rust
let _guard = Guard::enter()?;
while !self.quit {
    if redraw { /* sync highlight cache, render::draw, flush OSC 52 */ }
    if event::poll(POLL)? {            // POLL = 250 ms
        let ev = event::read()?;
        changed = !is_plain_mouse_motion(&ev);
        self.handle_event(ev);
    }
    let ticked = self.tick(now);       // recovery file
    redraw = self.expire_status(now) || changed || ticked;
}
```

- **Redraw only when needed.** A frame is drawn after an event, when a message fades, or when
  the recovery tick has something to say. Mouse motion is ignored, because capture reports every
  move and nothing uses it.
- **The 250 ms poll** lets the loop wake up without input, to fade messages (after 4 s) and to
  write the recovery file.

`handle_event` routes each key to exactly one owner, in this order:

1. the full-screen message (any key quits),
2. the command palette, if open,
3. the find session, if active,
4. the open prompt, if any,
5. the keymap: a bound chord becomes a `CommandId` and goes to `dispatch`,
6. otherwise, a printable character is typed into the buffer.

Paste follows the same idea: into find, into a prompt, or into the buffer. After every event,
`fit_width` adjusts the gutter (the line-number column can grow a digit) and
`layout::scroll_to_cursor` keeps the cursor on screen.

`dispatch(id)` is one big `match` over `CommandId`. Motion commands compute the new position with
`cursor::*` and either move or extend the selection; the others call into `Editor`, `files.rs`,
`find.rs` or the palette.

## 6. The text model

`Buffer` (`buffer.rs`) holds the text in a `ropey::Rope`, a balanced tree of text chunks. Inserts
and deletes anywhere cost O(log n), and line ↔ char index conversion is fast.

The rope never holds the file's exact bytes. On load, `Buffer::from_str_detect`:

1. strips a UTF-8 BOM and remembers it,
2. counts `\r\n` against lone `\n` and remembers the majority as the line ending,
3. turns every `\r\n` into `\n` (a lone `\r` stays as a literal character),
4. removes one final newline and remembers whether there was one.

All of that goes into `TextMeta { bom, line_ending, final_newline }`. `Buffer::to_bytes` reverses
the steps exactly, so a file you open and save without edits comes back byte for byte. Because the
rope has no trailing newline, "the number of lines" is simply `rope.len_lines()`.

Two more fields keep caches honest:

- `version` goes up on every real change. Layout caches are keyed by it.
- `changed_from` is the lowest line changed since the highlighter last looked
  (`take_changed_from`). The highlight cache uses it to forget only what an edit could affect.

## 7. Cursor and graphemes

A position is `Pos { line, col }`, where `col` is a **char** index within the line (not bytes, not
screen cells). All motion in `cursor.rs` is pure: it takes the rope and a `Pos` and returns a new
`Pos`.

Moving left or right steps one **grapheme cluster**, not one char, so `é` written as `e` + accent,
an emoji with a skin tone, or a flag all move as one unit. Segmentation runs over the rope chunks
of a single line, so a step costs about one grapheme, not the whole line. Word motion
(Ctrl+Left/Right) groups graphemes into words, punctuation and whitespace runs.

Screen width is a separate question, answered in layout: a CJK character or most emoji take two
cells, a tab expands to the next tab stop.

`Editor.preferred_col` remembers the column you were aiming for when moving up and down through
shorter lines, so the cursor returns to it on a longer line.

## 8. Editing and undo

`Editor` (`edit.rs`) bundles everything a text change touches:

```rust
pub struct Editor {
    pub buf: Buffer,
    pub cursor: Pos,
    pub anchor: Option<Pos>,       // selection = anchor..cursor
    pub preferred_col: Option<usize>,
    pub indent: Indent,            // what Tab inserts
    undo: UndoStack,
    now: Option<Instant>,          // pinned clock for tests
}
```

Every change, from typing to replace-all, ends up as one or more `Edit` records:

```rust
pub struct Edit {
    pub at: usize,          // char index
    pub removed: String,
    pub inserted: String,
    pub cursor_before: Pos,
    pub cursor_after: Pos,
}
```

Undo applies the inverse (remove `inserted`, put back `removed`) and restores `cursor_before`;
redo re-applies and restores `cursor_after`. Storing text instead of whole snapshots keeps memory
small even for big files.

`UndoStack` groups edits so one Ctrl+Z undoes a sensible amount:

- each edit has a kind: `Typing`, `Deleting` or `Other`,
- a new edit joins the previous group only if it has the same kind, comes within 1 second of
  the last one, and the buffer was not saved in between,
- typing must continue exactly where the last insert ended, and a space typed after a word
  closes the group, so undo removes about one word at a time,
- deleting must continue from the same spot (Delete) or just before it (Backspace),
- `Other` edits (paste, cut, replace, Enter…) never merge,
- `push_joined` forces an edit into the last group, so a change spanning several places (such as
  replace-all) undoes in one step,
- a new edit after some undos throws away the redo history.

The stack also remembers **the saved point** (`saved`). When undo or redo lands exactly on it,
the buffer counts as clean again, the title drops `Modified` and the recovery file is removed. If
the history that held the saved point is thrown away, it is gone for good (`None`).

`indent.rs` holds the rules for Enter and Tab: Enter keeps the line's indent and continues
Markdown list items (`- `, `* `, `1. `, `- [ ] `), and ends the list on an empty item. On load,
`detect_indent` looks at the first lines of the file: mostly tabs means Tab inserts a tab;
otherwise the most common step between indented lines (2 to 8) becomes the indent width. With
no clear answer it falls back to 2 for `.json` files and 4 for the rest. Makefiles always use
tabs.

## 9. Commands, keymap and palette

`commands.rs` holds `COMMANDS`, a static table with one `CommandInfo` per action:

```rust
CommandInfo {
    id: CommandId::Save,
    name: "Save",            // shown in the palette and --help
    config_key: "save",      // used in [keys]
    default_keys: &["ctrl+s"],
    hidden_keys: &[],        // bound but not shown (F1 for the palette)
}
```

Everything else reads this table:

- **`Keymap::defaults()`** parses every default and hidden chord into a
  `HashMap<KeyChord, CommandId>`. `lookup` turns a crossterm `KeyEvent` into a command; it
  normalises the event first (for example, an uppercase letter is Shift + that letter) and ignores
  key-release events.
- **The config** calls `set_keys(id, chords)` for each `[keys]` entry. That removes the command's
  old chords and steals each new chord from whichever command had it.
- **`keys_for(id)`** returns the visible chords for display: registry defaults first, in order,
  then any added by the config. The palette and `--help` both use it, so they always show the
  real keys.
- **The palette** lists every command except itself. `palette::score` is a small fuzzy matcher:
  the query's letters must appear in order (case-insensitive); consecutive hits score +10, hits at
  the start of a word +8, and every skipped char in between costs 1. Results are sorted by score.

Because every action is a `CommandId`, a command picked in the palette runs through the same
`dispatch` as its key.

## 10. Layout: from lines to screen rows

`layout.rs` maps buffer lines to screen rows and back. The `Viewport` holds what is on screen:

- `top: Row`: the first visible row (a line plus the column range shown on that row),
- `width`, `height`, `wrap`, `tab_width`,
- in no-wrap mode, the horizontal scroll and an **anchor** (`hscroll_col` on `hscroll_line`) so
  measuring "how far right is the cursor" starts near the cursor, not at column 0.

**Wrap mode.** A long line splits into rows at grapheme boundaries, by cell width. Walking is done
with `Cells`, a grapheme iterator over rope chunks that can start at any column, so the cost
follows the rows you ask for. The only step that reads a whole line is `WrapCache`: the row start
columns of one wrapped line, computed once per (line, width, tab width, buffer version). Moving up
and down inside a huge wrapped line reuses it instead of re-measuring.

**No-wrap mode.** One line is one row. The view scrolls sideways to follow the cursor; a wide
grapheme cut by either edge is left out and the gap padded.

Key functions:

- `visible_rows(rope, viewport)`: the rows to draw this frame,
- `cursor_screen(rope, viewport, cursor)`: where the terminal cursor goes,
- `scroll_to_cursor(...)`: moves `top` so the cursor is visible,
- `draw_cells(...)`: the graphemes of one row that fit, with the same rules as wrapping, so what
  is drawn always matches what was measured.

`Viewport::invalidate` drops caches when the buffer is replaced (open, reload), because `version`
restarts at 0 for a new buffer.

## 11. Rendering a frame

`render::draw` takes an `AppView`, a read-only snapshot of everything a frame needs, borrowed
from `App`. It draws the whole screen every time:

1. hide the cursor,
2. **title bar** (row 0, reverse video): version, file name, and the state (`Modified`,
   `New File`, the last-saved time, `[Read-only]`),
3. **text rows**: gutter (line numbers, if on) and the row's cells, coloured by the highlighter;
   the rest of the row is cleared with `Clear(UntilNewLine)`,
4. **selection** and find matches (reverse video for the current match, underline for others),
5. **status line** (last row): prompt, or a message,
6. the **message box** at the bottom right, unless it would cover the cursor or the terminal is too
   small (then the message goes to the status line),
7. the **palette** box, if open,
8. place and show the cursor.

All output is `queue!`d into a 64 KB `BufWriter` and written in one flush, so the terminal never
shows a half-drawn frame. Control characters in the text are drawn as a visible mark (`clean`),
so a stray escape byte in a file cannot mess up the terminal.

Colours are the terminal's 16 ANSI colours only, so nib follows your theme:

| Style | Colour | Used for |
|---|---|---|
| `Heading`, `Literal` | magenta | Markdown headings; JSON `true` / `false` / `null` |
| `Emph`, `Number` | yellow | Markdown emphasis; JSON numbers |
| `Code`, `String` | green | Markdown code; JSON strings |
| `Link`, `Key` | blue | Markdown links; JSON object keys |
| `ListMark` | cyan | Markdown list markers |
| `Punct` | dim | JSON brackets, commas, colons |

## 12. Syntax highlighting

`highlight/` holds two small hand-written highlighters. No grammar engine, no regex crate: each
works line by line and returns styled byte ranges.

**Choosing a language.** `lang_for(path)` looks at the extension, in any case: `.md` and
`.markdown` are Markdown, `.json` is JSON. Anything else is drawn plain.

**Markdown** (`markdown.rs`): `#` headings, `**bold**` / `*em*` / `_em_`, `` `code` `` and fenced
code blocks, `[text](url)` links, and list markers.

**JSON** (`json.rs`): keys (a string followed by `:`), other strings, numbers, `true` / `false` /
`null`, and punctuation. JSON needs no memory between lines.

**State between lines.** Markdown does: a line inside a ```` ``` ```` fence is code, not
Markdown. That one bit is `LineState { in_fence }`, passed from each line to the next.

**The cache.** To draw the first visible line correctly, nib must know whether it is inside a
fence, which depends on every line above. Rescanning from the top each frame would be slow in a
long file, so `highlight::Cache` remembers the line numbers of every fence above the viewport.
`state_at(line)` counts fences above it: odd means inside. Before each frame,
`Cache::sync(buf, lang, top)`:

1. reads the buffer's `changed_from` and forgets fences at or below it,
2. scans only the new lines up to `top`, reading just the first 6 chars of each (enough to spot a
   fence).

Scrolling down scans each line once; an edit only rescans from the edit down.

**Huge lines.** Only the first 10 000 chars of a line are highlighted (`LINE_LIMIT`), so a giant
minified JSON line costs a fixed amount per frame. The rest is drawn plain.

Within a frame, a wrapped line is highlighted once and the result reused for all its rows.

## 13. Find and replace

`FindSession` (`find.rs`) owns the Find and Replace interaction while it is open; `App` routes
keys and pastes to it.

- **Incremental.** Every change to the query searches again from where the cursor was when Find
  opened, forward and wrapping, and selects the match.
- **Smart case.** Plain text, case-insensitive unless the query has a capital letter.
- **Enter / Shift+Enter** go to the next / previous match. Matches on screen are underlined; the
  current one is reverse video.
- **Replace** asks `Find:`, then `Replace with:`, then visits each match from the cursor onward:
  (R)eplace (one undo step each), (A)ll (all remaining as one undo step), (S)kip, or Esc. The
  search resumes after the inserted text, so a replacement that contains the query cannot loop.
- After Find closes, its query is kept in `App.last_search` for the *Find Next* / *Find Previous*
  commands.

## 14. Prompts and file flows

`Prompt` (`prompt.rs`) is a one-line input on the status line: text entry (`Save as: `) or a
choice (`(Y/n)`). It returns a `PromptResult` (submitted text, a choice letter, or cancel).

What the prompt is **for** is stored next to it: `App.prompt: Option<(Prompt, PromptPurpose)>`.
`PromptPurpose::File(FileFlow)` drives the file flows in `files.rs`, which is a small state
machine:

| `FileFlow` | Asks |
|---|---|
| `SaveChoice { then }` | `Save? (Y)es / (N)o / (C)ancel` before leaving a dirty buffer |
| `SaveName { then }` | the name for an unnamed buffer, or Save As |
| `ConfirmReplace { path, then }` | `<name> exists. Replace? (y/N)` |
| `ExternalChange { path, then }` | `File changed on disk — overwrite? (y/N)` |
| `OpenName` | `Open: ` |
| `ReadOnly(..)` | `already open in another nib … open read-only? (Y/n)` |
| recovery | `Recover unsaved changes from …? (Y/n)` |

`Then` (`Stay`, `Quit`, `Open`) says what happens after a successful save. So "Quit on a dirty,
unnamed buffer" is: `SaveChoice { then: Quit }` → Y → `SaveName { then: Quit }` → name typed →
maybe `ConfirmReplace` → save → quit. Any failure or cancel leaves the current buffer as it was.

## 15. Loading files

`fileio::load(path, allow_large)` classifies a path before reading it:

| Result | When |
|---|---|
| `NotFound` | no such file: becomes a `New File` buffer, created on first save |
| `Directory` | a folder |
| `NotRegular` | a FIFO, device or socket (reading could block forever) |
| `TooLarge(size)` | over 50 MB, unless the user said yes |
| `Binary` | a NUL byte in the first 8 KB, or not valid UTF-8 |
| `Text(Buffer)` | everything else |

At startup a file that cannot be edited shows a full-screen message (`screens.rs`) instead of the
editor. From Ctrl+O the current buffer stays and the status line says why.

After a load nib records the file's modification time (`disk_mtime`) and whether it can write to
it. A file it cannot write opens with `[Read-only]`; Save then says to use Save As.

## 16. Saving: the atomic write

Every disk write in nib, both saves and recovery files, goes through `fileio::atomic_write`. A
save can fail, but it can never leave a half-written file:

1. **Follow symlinks** to the real file (up to a fixed number of hops), so saving through a link
   updates the target and the link stays a link.
2. **Create a temp file** next to the target: `.<name>.nib-<pid>.tmp` (with `-1`, `-2`… if taken),
   opened with `create_new`, so nib never overwrites a file it did not make.
3. **Write all bytes and `fsync`** them to disk.
4. **Copy permissions** from the original (and, on Unix, try to keep the owner and group).
5. **Rename** the temp file over the target. On every supported system, rename within one folder
   replaces the file in a single step: anyone reading sees the whole old file or the whole new one.
6. On Unix, **`fsync` the folder** so the rename itself survives a power cut.

If any step fails, the temp file is removed and the original is untouched.

Before saving, `files.rs` compares the file's current mtime with `disk_mtime`. If something else
changed the file since nib loaded or saved it, nib asks before overwriting. It also checks write
permission again, so fixing permissions while nib is open makes Save work.

## 17. Crash recovery and the file lock

`recovery.rs` makes sure a crash costs at most a few seconds of typing.

**Where.** `~/.local/share/nib/recovery/` on Linux, `%LOCALAPPDATA%\nib\recovery\` on Windows,
created when first needed. A named file uses `<hash>.rec`, where the hash is FNV-1a 64 of the
absolute path. An unnamed buffer uses `untitled-<pid>.rec`.

**What.** A small header (`nib-recovery 1`, `pid <n>`, `path <path>`, a blank line) followed by
the exact bytes a save would write.

**When.** `Recovery::tick` runs every loop turn. While the buffer is dirty it writes the recovery
file through `atomic_write`, at most every 3 seconds and only if the text changed since the last
write. When the buffer becomes clean (saved, or undone back to the saved point) the file is
removed. On a fatal error, `App::on_fatal_error` flushes it one last time.

**The lock.** While a named file is open, nib keeps `<hash>.lock` (same header, no text). When
nib opens a file, `Recovery::check` looks at the lock and the recovery file:

- **owned by a running nib** (the pid is alive and not ours) → offer to open read-only,
- **left by a nib that is gone** → offer to recover. Yes loads the saved text as a modified
  buffer; No moves the file to `<hash>.rec.bak` so nothing is lost,
- **nothing** → open normally.

"Is this pid alive?" is `/proc/<pid>` on Linux and `tasklist` on Windows. nib only ever
overwrites or deletes recovery and lock files whose header holds its own pid.

## 18. Clipboard

`clipboard.rs` defines a small trait, `Clip`, with two implementations:

- **`System`** uses `arboard` for the real system clipboard. It is used on Windows, and on Linux
  when `DISPLAY` or `WAYLAND_DISPLAY` is set and the clipboard opens.
- **`Osc52`** is the fallback (SSH, bare console). Copy sends the escape sequence
  `ESC ] 52 ; c ; <base64> BEL`, which asks the terminal to set its clipboard. Payloads over
  100 KB of base64 are not sent, because some terminals drop them; nib says so instead. Paste
  returns nib's own last copy, since a terminal cannot be asked for its clipboard safely.

The OSC 52 sequence is never written in the middle of a frame. `set` only stores it as
*pending*, and the event loop writes it right after the frame flush (`take_pending`).

## 19. Configuration

`config.rs` reads one TOML file from `config::path()` (`dirs::config_dir()/nib/config.toml`) at
startup. `serde` with unknown fields denied turns it into settings plus a `[keys]` table; each key
entry is a chord or a list of chords, parsed by `keymap::parse_chord` and applied with
`Keymap::set_keys`.

It is **all or nothing**. Invalid TOML, an unknown setting, command or key name, a value of the
wrong type or out of range, or one chord set for two commands: nib throws the whole file away,
uses all defaults, and shows `Config error: <message> — using defaults`. A half-applied config
would be harder to understand than none.

## 20. Testing

Run all three before calling a change done:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

How the tests are built:

- **Unit tests live next to the code** in each module's `#[cfg(test)] mod tests`.
- **Disk tests use `tempfile`.** Every path that writes to disk has a test in a temp folder:
  atomic write keeps permissions and symlinks, a failed write leaves the original and no temp
  file, recovery files appear and disappear at the right times.
- **The app is tested without a terminal.** `app::test_support` builds an `App` on a temp file
  and feeds it crossterm `Event`s directly (`key(...)`, mouse events), then checks the buffer,
  cursor, status or file on disk. `handle_event` never touches the terminal, which is what makes
  this possible.
- **Clocks are pinned.** Undo grouping, double-click and recovery timing take an explicit
  `Instant` in tests (`Editor::set_now`, `handle_mouse_at(m, now)`, `Recovery::tick(now, ..)`),
  so tests never sleep and never flake.
- **The process id check is injected.** `Recovery::new(dir, alive)` takes the "is this pid
  alive?" function, so tests can fake another running nib.

## 21. Build and release

- `cargo build --release` writes `target/release/nib` (or `nib.exe`) inside the repo;
  `.cargo/config.toml` pins the target folder there.
- The release profile is tuned for a small binary: `opt-level = "s"`, LTO, one codegen unit,
  stripped symbols, `panic = "abort"`. `unsafe_code` is forbidden crate-wide.
- **CI** (`.github/workflows/ci.yml`) runs fmt, clippy and tests on Linux and Windows for every
  push to `main` and every pull request.
- **Releases** (`.github/workflows/release.yml`) run when a tag `vX.Y.Z` is pushed. The tag must
  match the version in `Cargo.toml`.
  - Linux job: static `x86_64-unknown-linux-musl` build, checked to be statically linked, then
    the `.tar.gz`, `.deb` (`cargo-deb`) and `.rpm` (`cargo-generate-rpm`).
  - Windows job: `x86_64-pc-windows-msvc` with a static C runtime (so the zip runs without the
    VC++ redistributable), then the `.zip` and the `.msi` (`cargo-wix`, from `wix/main.wxs`).
  - Release job: checks all five files exist, writes `SHA256SUMS`, publishes the GitHub release.
- `install.sh` and `install.ps1` download from the latest release and check the download
  against `SHA256SUMS` before installing.

To cut a release: bump `version` in `Cargo.toml`, date the `CHANGELOG.md` entry, commit, then
`git tag -a vX.Y.Z -m "nib X.Y.Z"` and `git push origin vX.Y.Z`.

## 22. Recipe: adding a command

Say you want a *Duplicate Line* command on Ctrl+D.

1. **Spec first.** Add it to the keys table in `SPEC.md` §4.
2. **Registry.** Add `DuplicateLine` to `CommandId` and a `CommandInfo` to `COMMANDS` with
   `name: "Duplicate Line"`, `config_key: "duplicate_line"`, `default_keys: &["ctrl+d"]`.
   The keymap, palette, config and `--help` pick it up from there.
3. **Behaviour.** Add the text change as a method on `Editor` in `edit.rs`, recording an `Edit`
   so it undoes. Then handle `C::DuplicateLine` in `App::dispatch`.
4. **Tests.** A unit test for the `Editor` method (text, cursor, undo) and, if it has a message,
   an app-level test through `test_support`.
5. **Docs.** Add it to the key list in `README.md` if it is a main key, and to `CHANGELOG.md`.

`Keymap::defaults()` panics on a default chord that does not parse, and a test checks that
every `config_key` is unique, so those typos fail `cargo test` right away. A chord that another
command already uses is **not** caught: the later entry silently wins in the map. Check the keys
table in the spec before picking one.
