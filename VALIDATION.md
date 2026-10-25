# Validation status — 2026-10-25

- Source tree built and tested on Windows with the stable Rust 1.95.0 toolchain
  (`x86_64-pc-windows-msvc`).
- Passing local gates: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --all-targets`
  (125 tests: 108 unit, 16 CLI/integration, 1 fake-provider E2E), `cargo build --release`,
  `jani --version`, `jani --help`, `jani doctor`, `jani sandbox doctor`.
- Scope: Jani is a standalone coding agent. It has no coordination engine, identity
  control plane, message bus, merge authority or external coordinator.
- Implemented trust controls: signed tenant-bound policy and identity, default-deny
  PDP/PEP, work-contract narrowing, data classification and fail-closed model egress,
  a mediating tool broker, hash-chained audit journal, persisted budgets,
  exact-binding approvals and OS sandbox selection without silent fallback.
- Review hardening: provider credentials are stripped from every child process;
  commands cannot use `git --no-index`, `cargo clippy --fix`, combined short git flags,
  or out-of-scope commit pathspecs; commits refuse pre-staged files outside the approved
  edit scope; credential-path and symlink protections are covered by tests; Windows file
  replacement is supported. Policy rules that reference roles are rejected because
  identity carries no roles. Empty provider/region/purpose egress allowlists deny, with
  `"*"` as the only wildcard. Sandbox receipts report network confinement.
- E2E evidence: the fake-provider test runs the real CLI against a temporary worktree,
  asserts the API key never appears in a provider request, verifies the journal chain,
  and checks the persisted budget ledger.
- **Partial against the enterprise proposal pack.** The requirement-by-requirement
  review is in `docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md`. Plugins are
  verified but not executable; Windows and macOS cannot satisfy enterprise isolation
  profiles; there is no IdP/SCIM, policy administration, signed execution evidence,
  encrypted tenant storage, checkpoint/recovery or offline mode.
- **Not end-to-end verified:** no AI-provider key or live coding session was supplied
  for testing. Linux/bwrap execution and the `cfg(unix)` test path were not run on
  this host. No sandbox-escape or crash-recovery trial was run.
- **Not production-approved:** follow `docs/TESTING.md` before relying on it for real
  work. Do not represent this build as enterprise-ready or as an OS sandbox on
  Windows or macOS.
