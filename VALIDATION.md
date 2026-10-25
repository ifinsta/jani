# Validation status — 2026-10-25

- Source tree built and tested on Windows with the stable Rust 1.95.0 toolchain
  (`x86_64-pc-windows-msvc`).
- Passing local gates: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all-targets` (13 unit tests + 4 CLI tests), `cargo build --release`,
  `jani --version`, `jani --help`, and `jani doctor`.
- Scope: Jani is a standalone coding agent. It has no coordination engine, identity
  system, message bus, merge authority or external coordinator. File writes and command
  execution are default-denied and enforced in Rust.
- Review hardening: provider credentials are stripped from every child process;
  commands cannot use `git --no-index`, `cargo clippy --fix`, combined short git flags,
  or out-of-scope commit pathspecs; commits refuse pre-staged files outside the approved
  edit scope; credential-path and symlink protections are covered by tests; Windows file
  replacement is supported.
- **Not end-to-end verified:** no AI-provider key or live coding session was supplied for
  testing.
- **Not production-approved:** follow `docs/TESTING.md` before relying on it for real work.
