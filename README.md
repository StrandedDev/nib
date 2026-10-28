# nib

A small, fast, distraction-free text editor for the terminal, in the spirit of nano.

nib is built for notes and config files. It never loses or corrupts a file: every save is atomic,
unsaved changes are written to a recovery file while you work, and a second nib on the same file
opens it read-only. There is no shortcut bar; press **Ctrl+P** for a command palette that lists
the commands and their keys. Markdown and JSON are highlighted in your terminal's own colours.

```
 nib 0.1.0                    todo.md                    2026-10-27 14:02
 # Today

 - [ ] Renew passport
 - [x] Back up laptop
 - [ ] Call the plumber


```

## Supported systems

- Debian, Ubuntu, Fedora (x86_64)
- Windows 10 / 11 (x64)

## Install, update, uninstall

Releases are on the [GitHub releases page](https://github.com/StrandedDev/nib/releases). Every
release ships `SHA256SUMS`; the install scripts check downloads against it.

### Linux: install script

```sh
curl -fsSL https://raw.githubusercontent.com/StrandedDev/nib/main/install.sh | sh
```

On Debian and Ubuntu this installs the `.deb` with `apt`; on Fedora, the `.rpm` with `dnf`. nib
lands in `/usr/bin`. On any other distro the script falls back to a user install (below).

- **Update:** run the same command again.
- **Uninstall:** `sudo apt remove nib` (Debian, Ubuntu) or `sudo dnf remove nib` (Fedora).

### Linux: user install, no root

```sh
curl -fsSL https://raw.githubusercontent.com/StrandedDev/nib/main/install.sh | sh -s -- --user
```

Puts nib in `~/.local/bin` and adds that directory to `PATH` in `~/.profile`, `~/.bashrc` and
`~/.zshrc`. Open a new terminal afterwards, or run `hash -r` (bash) / `rehash` (zsh).

- **Update:** run the same command again.
- **Uninstall:** `curl -fsSL https://raw.githubusercontent.com/StrandedDev/nib/main/install.sh | sh -s -- --uninstall`
  removes the binary and the `PATH` lines. Use `--purge` instead to remove your config too.

### Linux: packages by hand

Download from the releases page, then:

| Package | Install / update | Uninstall |
|---|---|---|
| `nib_0.1.0_amd64.deb` | `sudo apt install ./nib_0.1.0_amd64.deb` | `sudo apt remove nib` |
| `nib-0.1.0.x86_64.rpm` | `sudo dnf install ./nib-0.1.0.x86_64.rpm` | `sudo dnf remove nib` |
| `nib-0.1.0-x86_64-linux.tar.gz` | extract and copy `nib` onto your `PATH` | delete the `nib` binary |

The Linux binary is statically linked, so the tarball runs on any x86_64 distro.

### Windows: install script

In PowerShell:

```powershell
irm https://raw.githubusercontent.com/StrandedDev/nib/main/install.ps1 | iex
```

Downloads the `.msi`, checks it, and installs nib for all users into Program Files (approve the
administrator prompt). nib is added to `PATH`; open a new terminal to use it.

- **Update:** run the same command again; the new version replaces the old one in place.
- **Uninstall:** *Settings → Apps → Installed apps* (or *Add or remove programs*), select nib, Uninstall.

### Windows: by hand

- **Installer:** download `nib-0.1.0-x86_64.msi` and run it. To update, run a newer `.msi`.
  Uninstall from *Add or remove programs*.
- **Portable:** download `nib-0.1.0-x86_64-windows.zip`, extract `nib.exe` anywhere on your
  `PATH`. To update, replace `nib.exe`; to uninstall, delete it.

### SmartScreen

nib's Windows builds are not code-signed, so when you run a downloaded `.msi` or `nib.exe`,
Windows SmartScreen may show *Windows protected your PC*. Click **More info**, then **Run anyway**.

### From source

With Rust 1.85 or later:

```sh
cargo install --git https://github.com/StrandedDev/nib
```

Run the same command with `--force` to update; `cargo uninstall nib` removes it.

## Usage

```
nib                 empty buffer; the first save asks for a name
nib notes           new file notes.txt (created on first save)
nib todo.md         opens todo.md, or creates it on first save
nib app.log:42      opens app.log at line 42
nib --help          prints help
nib --version       prints the version
```

- A new file name without an extension gets `.txt`, unless it starts with a dot or is
  `Makefile`, `Dockerfile`, `LICENSE` or `README`. Existing files always open as named.
- `path:42` opens at line 42 only when no file is literally named `path:42`. Use `--` before a
  file name that starts with `-`.
- The title bar shows the file's last-saved time, `Modified`, or `New File`, plus `[Read-only]`
  when you cannot write to it.
- **Find** (Ctrl+F) is plain text and case-insensitive unless the query has a capital letter.
  **Replace** (Ctrl+R) asks for each match: (R)eplace, (A)ll, (S)kip or Esc.
- Enter keeps the line's indent and continues Markdown lists (`- `, `* `, `1. `, `- [ ] `);
  Enter on an empty list item ends the list. Tab inserts spaces to the detected indent width
  (real tabs in files that use them, and in Makefiles).
- Mouse: click to place the cursor, drag to select, double-click to select a word, wheel to scroll.
- Copy and cut go to the system clipboard. Without a display (over SSH, or on a bare console)
  they go to your terminal's clipboard through OSC 52, and Ctrl+V pastes nib's last copy; use
  your terminal's own paste for anything else.
- Each file keeps its own line endings (LF or CRLF), its UTF-8 BOM and its final newline.
- If nib or your terminal crashes, the next time you open the file nib offers to recover the
  unsaved changes. Recovery files live in `~/.local/share/nib/recovery/` (Linux) or
  `%LOCALAPPDATA%\nib\recovery\` (Windows).

## Keys

Every key can be changed in the [config](#configuration). Defaults:

| Action | Key | Config name |
|---|---|---|
| Command palette | Ctrl+P (also F1) | `palette` |
| Save | Ctrl+S | `save` |
| Save As | palette only | `save_as` |
| Open file | Ctrl+O | `open` |
| Quit | Ctrl+Q | `quit` |
| Find | Ctrl+F | `find` |
| Find next / previous (in Find) | Enter / Shift+Enter | |
| Find Next / Find Previous (after Find) | palette only | `find_next` / `find_prev` |
| Replace | Ctrl+R | `replace` |
| Go to line | Ctrl+G | `goto_line` |
| Undo | Ctrl+Z | `undo` |
| Redo | Ctrl+Y, Ctrl+Shift+Z | `redo` |
| Copy | Ctrl+C | `copy` |
| Cut (the whole line when nothing is selected) | Ctrl+X | `cut` |
| Paste | Ctrl+V | `paste` |
| Select all | Ctrl+A | `select_all` |
| Toggle wrap | palette only | `toggle_wrap` |
| Toggle line numbers | palette only | `toggle_line_numbers` |
| Move | Left / Right / Up / Down | `left` / `right` / `up` / `down` |
| Word left / right | Ctrl+Left / Ctrl+Right | `word_left` / `word_right` |
| Line start / end | Home / End | `home` / `end` |
| Page up / down | PageUp / PageDown | `page_up` / `page_down` |
| File start / end | Ctrl+Home / Ctrl+End | `file_start` / `file_end` |
| Select left / right / up / down | Shift+Left / Shift+Right / Shift+Up / Shift+Down | `sel_left` / `sel_right` / `sel_up` / `sel_down` |
| Select word left / right | Ctrl+Shift+Left / Ctrl+Shift+Right | `sel_word_left` / `sel_word_right` |
| Select to line start / end | Shift+Home / Shift+End | `sel_home` / `sel_end` |
| Backspace / Delete | Backspace / Delete | `backspace` / `delete` |
| Delete word back / forward | Ctrl+Backspace / Ctrl+Delete | `delete_word_back` / `delete_word_fwd` |
| New line | Enter | `newline` |
| Indent | Tab | `tab` |
| Cancel a prompt | Esc | |

Some legacy terminals send Shift+Enter as Enter. Ctrl+H is Backspace in many terminals, which is
why Replace is Ctrl+R.

## Command palette

Press **Ctrl+P** (or F1). The palette lists these commands with their current keys:

Save · Save As · Open File · Quit · Find · Find Next · Find Previous · Replace · Go to Line ·
Undo · Redo · Copy · Cut · Paste · Select All · Toggle Wrap · Toggle Line Numbers ·
Delete Word Back · Delete Word Forward

Type to filter (fuzzy and case-insensitive: `tln` finds *Toggle Line Numbers*). Up / Down or
Ctrl+P / Ctrl+N move the selection, Enter runs the command, Esc closes the palette.

## Configuration

nib reads one TOML file at startup:

- Linux: `~/.config/nib/config.toml` (or `$XDG_CONFIG_HOME/nib/config.toml`)
- Windows: `%APPDATA%\nib\config.toml`

Under `sudo`, root's config is used. Every entry is optional; a missing file means all defaults.
The values below are the defaults:

```toml
wrap = true            # soft wrap
line_numbers = false
tab_width = 4          # 1 to 8; unset by default. When set, replaces the detected indent
                       # width; files that indent with real tabs keep tabs, shown this wide
line_ending = "lf"     # "lf" | "crlf"; for new files only (existing files keep theirs)

[keys]
save = "ctrl+s"                    # one chord
redo = ["ctrl+y", "ctrl+shift+z"]  # or a list
```

- `[keys]` takes the config names from the [key table](#keys).
- A chord is any of the modifiers `ctrl`, `alt`, `shift` joined with `+` to one key (any case):
  a character, `f1`–`f12`, `enter`, `esc`, `tab`, `backspace`, `delete`, `insert`, `home`,
  `end`, `pageup`, `pagedown`, `up`, `down`, `left`, `right`, `space`. An uppercase letter means
  Shift plus that letter.
- An override replaces all of that command's default keys (including the hidden F1 for the
  palette). An empty list, `save_as = []`, leaves a command palette-only. A key you assign is
  taken away from whichever command had it by default.
- Any mistake (invalid TOML, an unknown setting, command or key name, a wrong type or
  out-of-range value, or one key set for two commands) ignores the whole file: nib starts with
  all defaults and shows `Config error: <message> — using defaults` in the status line.

## License

[MIT](LICENSE)
