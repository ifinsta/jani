# Validation status

**Verdict: supervised local prototype. Not production-approved and not enterprise-certified.**

This page answers one question: *what has actually been executed, and what does that evidence allow you to claim?* It is a snapshot, not a permanent certification. Source code and executable tests are the authority; a passing count here does not extend to platforms or providers that were never run.

Snapshot: 2026-10-25, Windows `x86_64-pc-windows-msvc`, Rust 1.95.0 pinned by `rust-toolchain.toml`.

## What is proven

| Gate | Command | Observed result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | Clean |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | Zero warnings |
| Tests | `cargo test --workspace --all-targets` | 125 passed, 0 failed: 108 unit, 16 CLI/integration, 1 fake-provider E2E |
| Release build | `cargo build --release -p jani` | Succeeds; `jani --version` and `jani --help` run |
| CLI smoke | `jani doctor`, `jani sandbox doctor` | Report worktree, skills, model, settings source, permissions and sandbox support |
| Architecture gate | `cargo run -p jani-check` | Passes |
| Documentation manifest | `python tools/doccheck.py` | 52 normative documents and the 12 mandatory core contracts validate |
| Claims register | `cargo run -p jani-claims -- check` | Register current; 0 unresolved deterministic findings |
| Synthetic corpus | `python -m synthetic.generate generate` / `verify` | 5,000,000 cases generated and integrity-verified locally |
| Native corpus | `python -m synthetic.native_tools generate` / `verify` | Generated and verified; CI exercises 2,400 cases |
| Release installer | `powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1` | `v0.1.0` installs after SHA-256 verification and reports its version; a tampered copy is refused before installation |
| CI matrix | `.github/workflows/ci.yml` | Ubuntu and Windows jobs run formatting, claims, docs, clippy, tests and release build |

### Security behavior covered by negative tests

The suite does not merely exercise happy paths. Executed tests deny or reject:

- Writes without `--write-path`, out-of-scope paths, traversal, symlink components and protected credential paths.
- Unsigned, expired, wrong-tenant or tampered policy and identity assertions; role-scoped rules; empty and wildcard egress allowlists.
- Secret-classified content leaving through model requests, even when redaction is disabled.
- Unapproved external writes and high-risk actions without a single-use, exactly bound approval.
- Journal tampering, broken hash chains and budget ledgers that would otherwise widen across restarts.
- Missing isolation profiles and unavailable sandboxes rather than falling back silently.
- MCP forged, expired, missing-role or wildcard delegations, tampered response receipts and agent use of user-only commit tools.
- Inline API keys and unknown settings fields; profile credentials never silently borrowing another provider's key.
- Credential inheritance by child processes.

The fake-provider E2E test runs the real CLI against a temporary worktree and asserts that the API key never appears in a provider request, the journal chain verifies and the persisted budget ledger is correct.

## Reproduce it

Run from the repository root on a disposable checkout:

```bash
cargo fmt --all -- --check
cargo run -p jani-check
cargo test -p jani-claims
python tools/doccheck.py
cargo run -p jani-claims -- check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release -p jani
```

Optional synthetic evidence (Python 3.11; outputs stay out of Git):

```bash
python -m unittest discover -s synthetic/tests -v
python -m synthetic.generate generate --output synthetic/generated/full_5m --count 5000000
python -m synthetic.generate verify --output synthetic/generated/full_5m
python -m synthetic.native_tools generate --output synthetic/generated/native_5m --count 5000000
python -m synthetic.native_tools verify --output synthetic/generated/native_5m
```

Record the commit SHA, platform, runner identity and artifact digests if you rely on these results. A rerun on another platform is new evidence, not a formality.

## What the evidence does not cover

- **No live model session.** No provider key or real coding task was supplied. The E2E test uses a deterministic fake provider. Provider behavior, rate limits, billing and tool-call quality remain untested.
- **No Linux execution on this host.** bubblewrap confinement and the `cfg(unix)` test path were not run here. CI targets Ubuntu, but a green CI job is not a penetration test.
- **No sandbox-escape or crash-recovery trial.** Confinement is asserted by selection and refusal tests, not by adversarial escape attempts.
- **No executed 5M corpus.** Generating and verifying inputs proves corpus integrity only. There are no per-case Jani execution receipts, so the corpus is not evidence of runtime correctness.
- **Release installer coverage is narrow.** Only the committed Windows x64 artifact was installed and exercised on this host. There is no code signing, no clean-machine test, no Linux artifact install and no auto-update or rollback test.
- **No independent signoff.** All evidence here was produced locally or in CI without an external reviewer. See the receipt and reviewer fields in [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md).
- **No enterprise controls.** There is no IdP/SCIM, policy administration, key rotation/revocation, encrypted tenant storage, signed execution receipt, plugin runtime or portable OS sandbox. See the [implementation review](docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md).

## Release decision

A missing critical gate is a blocker, not a caveat. The only defensible current use is a supervised, local, ideally read-only evaluation with disposable worktrees. Do not describe this build as enterprise production-ready or as an OS sandbox on Windows or macOS. Release criteria and required evidence fields are defined in [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md); testing workflow and trial coverage are in [docs/TESTING.md](docs/TESTING.md).

Keep this page current when gates, platforms or guarantees change. Never convert "generated", "planned" or "proposed" into "passed" without an execution record.
