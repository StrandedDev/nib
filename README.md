# ✒️ nib

**A small, fast, distraction-free text editor for the terminal, in the spirit of nano.**

nib is built for notes and config files. Open a file, type, press **Ctrl+S**, done. There is no
shortcut bar to memorise: press **Ctrl+P** and a command palette lists everything nib can do,
with its keys.

![nib editing a note](screenshots/basic.png)

## 📑 Contents

- [Highlights](#-highlights)
- [Quick start](#-quick-start)
- [Install](#-install)
- [Build from source](#-build-from-source)
- [Using nib](#-using-nib)
- [Navigating](#-navigating)
- [Command palette](#-command-palette)
- [All keys](#-all-keys)
- [Configuration](#-configuration)
- [Files nib keeps](#-files-nib-keeps)
- [License](#-license)

## ✨ Highlights

| | |
|---|---|
| 🛡️ **Never loses your work** | Every save is atomic: the file is either fully old or fully new, never half-written. |
| 💾 **Crash recovery** | Unsaved changes are written to a recovery file as you type. If nib or your terminal dies, nib offers them back next time. |
| 🔒 **Safe with two windows** | A second nib on the same file opens it read-only, so two copies can't overwrite each other. |
| 🎯 **Command palette** | **Ctrl+P** lists every command and its keys. Type a few letters to find one. |
| 🎨 **Syntax highlighting** | Markdown and JSON, in your terminal's own colours, so it matches your theme. |
| ⌨️ **Familiar keys** | Ctrl+S, Ctrl+C, Ctrl+V, Ctrl+Z… the keys you already know. All of them can be changed. |
| 🖱️ **Mouse support** | Click, drag to select, double-click a word, scroll with the wheel. |
| 📋 **Clipboard that works over SSH** | Uses the system clipboard, or your terminal's clipboard (OSC 52) when there is no display. |
| 📝 **Respects your files** | Keeps each file's line endings (LF / CRLF), BOM and final newline exactly as they were. |
| 🪶 **Tiny** | One small binary, no runtime, no plugins to manage. |

<table>
<tr>
<td><img src="screenshots/command_palette.png" alt="The command palette filtering commands by 'line'"></td>
<td><img src="screenshots/syntax_highlight.png" alt="JSON syntax highlighting"></td>
</tr>
<tr>
<td align="center">🎯 Command palette (<b>Ctrl+P</b>)</td>
<td align="center">🎨 JSON highlighting</td>
</tr>
</table>

## 🚀 Quick start

1. **Install** (pick your system):

   ```sh
   # 🐧 Linux
   curl -fsSL https://raw.githubusercontent.com/StrandedDev/nib/main/install.sh | sh
   ```

   ```powershell
   # 🪟 Windows (PowerShell)
   irm https://raw.githubusercontent.com/StrandedDev/nib/main/install.ps1 | iex
   ```

2. **Open a new terminal**, then open a file:

   ```sh
   nib notes.md
   ```

3. **Type** your text. Press **Ctrl+S** to save and **Ctrl+Q** to quit.

4. Forgot a key? Press **Ctrl+P** and type what you want, such as `find` or `undo`.

Run `nib --help` at any time for a summary of options, the main keys and where your config lives.

## 📦 Install

Supported systems:

- 🐧 Debian, Ubuntu, Fedora (x86_64). The Linux binary is statically linked, so the tarball
  runs on any x86_64 distro.
- 🪟 Windows 10 / 11 (x64)

Every download is on the [GitHub releases page](https://github.com/StrandedDev/nib/releases).
Each release ships a `SHA256SUMS` file, and the install scripts check downloads against it.

| File | For |
|---|---|
| `nib_X.Y.Z_amd64.deb` | Debian, Ubuntu |
| `nib-X.Y.Z.x86_64.rpm` | Fedora |
| `nib-X.Y.Z-x86_64-linux.tar.gz` | Any x86_64 Linux |
| `nib-X.Y.Z-x86_64.msi` | Windows installer |
| `nib-X.Y.Z-x86_64-windows.zip` | Windows, portable (no install) |

### 🐧 Linux: install script (recommended)

```sh
curl -fsSL https://raw.githubusercontent.com/StrandedDev/nib/main/install.sh | sh
```

On Debian and Ubuntu this installs the `.deb` with `apt`; on Fedora, the `.rpm` with `dnf`. nib
lands in `/usr/bin`. On any other distro the script falls back to a user install (below).

- 🔄 **Update:** run the same command again.
- 🗑️ **Uninstall:** `sudo apt remove nib` (Debian, Ubuntu) or `sudo dnf remove nib` (Fedora).

### 🐧 Linux: user install, no root

```sh
curl -fsSL https://raw.githubusercontent.com/StrandedDev/nib/main/install.sh | sh -s -- --user
```

Puts nib in `~/.local/bin` and adds that directory to `PATH` in `~/.profile`, `~/.bashrc` and
`~/.zshrc`. Open a new terminal afterwards, or run `hash -r` (bash) / `rehash` (zsh).

- 🔄 **Update:** run the same command again.
- 🗑️ **Uninstall:** removes the binary and the `PATH` lines. Use `--purge` instead of
  `--uninstall` to remove your config too.

  ```sh
  curl -fsSL https://raw.githubusercontent.com/StrandedDev/nib/main/install.sh | sh -s -- --uninstall
  ```

### 🐧 Linux: packages by hand

Download from the releases page, then:

| Package | Install / update | Uninstall |
|---|---|---|
| `.deb` | `sudo apt install ./nib_X.Y.Z_amd64.deb` | `sudo apt remove nib` |
| `.rpm` | `sudo dnf install ./nib-X.Y.Z.x86_64.rpm` | `sudo dnf remove nib` |
| `.tar.gz` | extract and copy `nib` onto your `PATH` | delete the `nib` binary |

### 🪟 Windows: install script (recommended)

In PowerShell:

```powershell
irm https://raw.githubusercontent.com/StrandedDev/nib/main/install.ps1 | iex
```

Downloads the `.msi`, checks it, and installs nib for all users into Program Files (approve the
administrator prompt). nib is added to `PATH`; open a new terminal to use it.

- 🔄 **Update:** run the same command again; the new version replaces the old one in place.
- 🗑️ **Uninstall:** *Settings → Apps → Installed apps* (or *Add or remove programs*), select nib,
  Uninstall.

### 🪟 Windows: by hand

- **Installer:** download the `.msi` and run it. To update, run a newer `.msi`. Uninstall from
  *Add or remove programs*.
- **Portable:** download the `.zip` and extract `nib.exe` into any folder on your `PATH`. To
  update, replace `nib.exe`; to uninstall, delete it.

> [!NOTE]
> 🛡️ **SmartScreen:** nib's Windows builds are not code-signed, so when you run a downloaded
> `.msi` or `nib.exe`, Windows may show *Windows protected your PC*. Click **More info**, then
> **Run anyway**.

## 🔧 Build from source

You need [Rust](https://rustup.rs) 1.85 or later.

**Install straight from GitHub** (puts `nib` in `~/.cargo/bin`, which rustup already adds to
`PATH`):

```sh
cargo install --git https://github.com/StrandedDev/nib
```

Run it again with `--force` to update; `cargo uninstall nib` removes it.

**Or clone and build:**

```sh
git clone https://github.com/StrandedDev/nib
cd nib
cargo build --release
```

The binary is written inside the repo folder:

| System | Path |
|---|---|
| 🐧 Linux | `target/release/nib` |
| 🪟 Windows | `target\release\nib.exe` |

Run it from there, or copy it into a folder on your `PATH`. To check a change, run
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`.

## 📝 Using nib

```
nib                 empty buffer; the first save asks for a name
nib notes           new file notes.txt (created on first save)
nib todo.md         opens todo.md, or creates it on first save
nib app.log:42      opens app.log at line 42
nib -- -draft.md    opens a file whose name starts with -
nib -h, --help      prints help: options, main keys, config location
nib -V, --version   prints the version
```

**The screen** has two parts:

- 🔝 **Title bar:** nib's version, the file name, and the file's state: its last-saved time,
  `Modified` once you change something, or `New File`. `[Read-only]` appears when you cannot write
  to the file.
- 💬 **Messages** such as `Saved notes.md`, `Copied 3 lines` or `Not found` pop up in a small box at
  the bottom right and fade after 4 seconds. Prompts such as `Find: ` use the bottom line.

**Good to know:**

- 📄 A new file name without an extension gets `.txt`, unless it starts with a dot or is
  `Makefile`, `Dockerfile`, `LICENSE` or `README`. Existing files always open as named.
- 🆕 New files are created on the first save, never just by opening nib.
- 📍 `path:42` opens at line 42 only when no file is literally named `path:42`.
- ↩️ Enter keeps the line's indent and continues Markdown lists (`- `, `* `, `1. `, `- [ ] `).
  Enter on an empty list item ends the list.
- ⇥ Tab inserts spaces to the file's indent width (real tabs in files that use them, and in
  Makefiles).
- 📋 Copy and cut go to the system clipboard. Without a display (over SSH, or on a bare console)
  they go to your terminal's clipboard through OSC 52, and Ctrl+V pastes nib's last copy; use your
  terminal's own paste for anything else.
- 💥 If nib or your terminal crashes, the next time you open the file nib offers to recover the
  unsaved changes.
- 🚪 Quitting or opening another file with unsaved changes asks
  `Save? (Y)es / (N)o / (C)ancel`.

## 🧭 Navigating

**Moving around**

| Do this | Press |
|---|---|
| Move by character / line | Arrow keys |
| Jump a word | Ctrl+Left / Ctrl+Right |
| Start / end of line | Home / End |
| Up / down a screen | PageUp / PageDown |
| Start / end of file | Ctrl+Home / Ctrl+End |
| Go to a line number | Ctrl+G, type the number, Enter |
| Put the cursor somewhere | Click with the mouse |
| Scroll | Mouse wheel |

**Selecting text**

| Do this | Press |
|---|---|
| Extend the selection | Shift + any movement key above |
| Select a word | Double-click it |
| Select with the mouse | Click and drag |
| Select everything | Ctrl+A |

**Finding and replacing**

- 🔍 **Find** (Ctrl+F): type and nib jumps to the first match as you go. Enter finds the next
  match, Shift+Enter the previous one, Esc closes. The search is plain text and ignores case unless
  your query has a capital letter.
- 🔁 **Replace** (Ctrl+R): enter the text to find, then the replacement. For each match choose
  **(R)**eplace, **(A)**ll, **(S)**kip or Esc to stop.
- After closing Find, *Find Next* and *Find Previous* in the palette repeat the last search.

**Views**

- 📏 *Toggle Line Numbers* and ↔️ *Toggle Wrap* (soft wrap of long lines) are in the palette, or
  can be set permanently in the [config](#-configuration).

## 🎯 Command palette

Press **Ctrl+P** (or F1). The palette lists every command with its current keys, so anything you
can do with a key you can also find by name.

- Type to filter. Matching is fuzzy and ignores case: `tln` finds *Toggle Line Numbers*.
- Up / Down (or Ctrl+P / Ctrl+N) move the selection, Enter runs the command, Esc closes the
  palette.
- A command run from the palette does exactly what its key does: *Move Down* moves the cursor down
  a line, *Indent* inserts an indent.

Some commands have no key by default and live only in the palette: *Save As*, *Find Next*,
*Find Previous*, *Toggle Wrap* and *Toggle Line Numbers*. You can give them keys in the config.

## 🔑 All keys

Every key can be changed in the [config](#-configuration). The *Config name* column is
the name to use there.

| Action | Default key | Config name |
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

> [!TIP]
> Some older terminals send Shift+Enter as plain Enter. Ctrl+H is Backspace in many terminals,
> which is why Replace is Ctrl+R.

## 🧰 Configuration

nib works without any config. To change settings or keys, create one TOML file:

| System | Config file |
|---|---|
| 🐧 Linux | `~/.config/nib/config.toml` (or `$XDG_CONFIG_HOME/nib/config.toml`) |
| 🪟 Windows | `%APPDATA%\nib\config.toml` |

`nib --help` prints the exact path on your system. nib reads the file at startup, so restart nib
after editing it. Under `sudo`, root's config is used.

**Step by step:**

```sh
# 🐧 Linux
mkdir -p ~/.config/nib
nib ~/.config/nib/config.toml
```

```powershell
# 🪟 Windows (PowerShell)
mkdir "$env:APPDATA\nib" -Force
nib "$env:APPDATA\nib\config.toml"
```

### Settings

Every entry is optional. The values below are the defaults:

```toml
wrap = true            # soft-wrap long lines
line_numbers = false   # show line numbers on the left
tab_width = 4          # 1 to 8; unset by default. When set, replaces the detected indent
                       # width; files that indent with real tabs keep tabs, shown this wide
line_ending = "lf"     # "lf" | "crlf"; for new files only (existing files keep theirs)
```

### Keys

Add a `[keys]` table. Each entry is a [config name](#-all-keys) set to one key or a
list of keys:

```toml
[keys]
save = "ctrl+s"                    # one key
redo = ["ctrl+y", "ctrl+shift+z"]  # or a list
save_as = "ctrl+shift+s"           # give a palette-only command a key
toggle_line_numbers = "f2"
quit = "ctrl+w"
```

- 🔤 A key is any of the modifiers `ctrl`, `alt`, `shift` joined with `+` to one key (any case):
  a character, `f1`–`f12`, `enter`, `esc`, `tab`, `backspace`, `delete`, `insert`, `home`,
  `end`, `pageup`, `pagedown`, `up`, `down`, `left`, `right`, `space`. An uppercase letter means
  Shift plus that letter.
- 🔁 Setting a command replaces all of its default keys (including the hidden F1 for the palette).
- 🚫 An empty list, such as `find = []`, makes a command palette-only.
- 🔀 A key you assign is taken away from whichever command had it by default.

### A complete example

```toml
# ~/.config/nib/config.toml
wrap = false
line_numbers = true
tab_width = 2

[keys]
save_as = "ctrl+shift+s"
toggle_wrap = "alt+z"
toggle_line_numbers = "alt+l"
```

> [!WARNING]
> Any mistake (invalid TOML, an unknown setting, command or key name, a wrong type or
> out-of-range value, or one key set for two commands) makes nib ignore the **whole** file. nib
> then starts with all defaults and shows `Config error: <message> — using defaults`, so you can
> fix it.

## 📂 Files nib keeps

| What | 🐧 Linux | 🪟 Windows |
|---|---|---|
| Config | `~/.config/nib/config.toml` | `%APPDATA%\nib\config.toml` |
| Recovery files | `~/.local/share/nib/recovery/` | `%LOCALAPPDATA%\nib\recovery\` |

nib asks before using a recovery file. If you decline, it is kept next to the others as
`<name>.rec.bak`, so nothing is thrown away.

## 📄 License

[MIT](LICENSE)
