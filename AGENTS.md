# nib — agent notes

`SPEC.md` is the source of truth for behaviour. Read the relevant section before changing a feature; update it in the same change when behaviour changes.

## Local toolchain (Windows dev machine)

- Rust lives on **N:**, not C: (C: is nearly full). `CARGO_HOME=N:\Dependencies\rust\cargo`, `RUSTUP_HOME=N:\Dependencies\rust\rustup`. Install any new tool (`cargo install …`, SDKs, caches) under `N:\Dependencies\`.
- Host toolchain is `stable-x86_64-pc-windows-gnu` (no MSVC Build Tools installed). CI and releases build MSVC/musl targets; local builds are gnu.
- Build output goes to `N:/Dependencies/rust/target` (set in `$CARGO_HOME/config.toml`) because the repo sits inside OneDrive. Look there for binaries, not `./target`.
- A shell started before the install may lack these env vars; prefix commands with them if `cargo` is not found.

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
