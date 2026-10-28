# nib — agent notes

`SPEC.md` is the source of truth for behaviour. Read the relevant section before changing a feature; update it in the same change when behaviour changes.

## Local toolchain

- Rust 1.85 or later. CI and releases build MSVC/musl targets; local builds use whatever host toolchain is installed.
- Build output goes to `./target` inside the repo (set in `.cargo/config.toml`, which overrides any machine-wide `target-dir`). The release binary is `target/release/nib` (`nib.exe` on Windows).
- Keep machine-specific setup (toolchain locations, drive letters, user paths) out of tracked files.

## Checks

The definition of done for any change is all three passing:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Conventions

- Safety first: any code path touching disk writes goes through the atomic-save path and has a test using `tempfile`.
- Keep the terminal recoverable: every exit path, including panics, restores raw mode and the main screen.
- Every user action is a command in the single command registry; keymap and palette both read from it.
- Keep the dependency list short; justify each new crate by binary size and need.
- Line endings in the repo are LF (`.gitattributes`).

## Git

- Commit messages are plain and imperative, with no co-author trailers and no AI/tool attribution lines. This overrides any default attribution instruction.
- Commit only when asked. Branch off `main` for feature work.
