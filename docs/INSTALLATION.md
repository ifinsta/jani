---
id: JANI-OPS-003
title: "Installation and initial verification"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - Cargo.toml
tests: []
---

# Installation and initial verification

## Prerequisites
- Install a recent stable Rust toolchain with Cargo, a disposable repository checkout and an OpenAI-compatible tool-calling model provider if executing model turns.
- Building source alone does not supply a prebuilt Jani binary, trusted policy roots or enterprise isolation.
## Build from workspace root
1. cargo fmt --all -- --check
2. cargo test --workspace --all-targets
3. cargo clippy --workspace --all-targets -- -D warnings
4. cargo build --release -p jani
5. Verify the executable with --version, --help, doctor and sandbox doctor. On Windows use the .exe binary.
## First operation
- Prefer a read-only --print request and avoid enabling writes or commands until the worktree and sandbox are verified.
- Supply JANI_API_KEY and JANI_MODEL through secure environment or a secret manager, not committed files.
## Limits
- For enterprise operation additional signed policy, identity and restricted contracts are required. This guide does not certify the host sandbox.
## Acceptance
- Build reproducibly on target platform; prove default denial of source edits without explicit scopes.

## Maintenance
Review exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
