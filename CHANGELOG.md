# Changelog

All notable changes to nib are listed here. Versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-28

First release.

- Terminal editor with a nano-style title bar, soft wrap and optional line numbers.
- Grapheme- and width-aware cursor movement, keyboard and mouse selection.
- Undo and redo grouped by word, system clipboard with an OSC 52 fallback, bracketed paste.
- Auto-indent, Markdown list continuation, indent width detection.
- Incremental find, replace, go to line, open and save as.
- Command palette showing each command's current keys.
- Markdown and JSON highlighting in the terminal's 16 ANSI colours.
- Atomic saves that keep line endings, BOM, final newline, symlinks and file mode.
- Recovery file for unsaved changes, and read-only opening when another nib has the file.
- Warning before overwriting a file changed on disk; message screens for files nib cannot edit.
- TOML config for settings and key overrides.
- `--help` (`-h`) with examples, the main keys as configured, and the config and recovery paths;
  `-V` for `--version`.
- Release builds: `.deb`, `.rpm`, Linux tarball, Windows `.msi` and `.zip`, with install scripts
  for Linux and Windows.
