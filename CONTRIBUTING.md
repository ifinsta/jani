---
id: JANI-GOV-001
title: "Contributing"
status: implemented-partial
owner: project
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - Cargo.toml
tests: []
---

# Contributing

Jani changes privilege, persistence and audit boundaries. The point of this process is to make those changes reviewable and to keep documentation tied to executable evidence. Read this before opening a large pull request.

## Before you write code

For anything beyond a small local fix, write a proposal first. State:

- The problem and the user-visible effect you expect.
- The trust boundary you are touching: authorization, egress, sandboxing, persistence, audit, the CLI parser, MCP delegation or native tools.
- The source modules and public contracts affected.
- Failure modes and how the change fails closed when a control is unavailable.
- Acceptance criteria, including the negative tests that must fail without the change.
- Platform scope, including what will remain deliberately unsupported.

Get agreement on the boundary before implementation. A proposal, plan or alert is not evidence that work is complete, and model output is not a design authority.

## Set up

- Rust 1.95.0, pinned by [rust-toolchain.toml](rust-toolchain.toml), with `rustfmt` and `clippy`.
- Python 3.11 for the synthetic corpus tools. Nothing else is required for the default test path.
- No provider key is needed: the E2E test uses a deterministic fake provider. Never commit a live key, private signing key, customer data or generated corpus shards.

## Fast local loop

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p jani-core
cargo run -p jani-check
```

Run the focused package or test while iterating, then the full gate below before requesting review. `--workspace` matters: the root workspace's default members exclude several crates, so a bare `cargo test` does not exercise them.

## Definition of done

This is the CI sequence from [.github/workflows/ci.yml](.github/workflows/ci.yml). It must pass locally, not only in CI.

```bash
python -m unittest discover -s synthetic/tests -v
python -m synthetic.generate generate --output synthetic/generated/ci_smoke --count 2500 --shard-size 500 --quiet
python -m synthetic.generate verify --output synthetic/generated/ci_smoke
python -m synthetic.native_tools generate --output synthetic/generated/native_ci --count 2400 --shard-size 400
python -m synthetic.native_tools verify --output synthetic/generated/native_ci
cargo fmt --all -- --check
cargo run -p jani-check
cargo test -p jani-claims
python tools/doccheck.py
cargo run -p jani-claims -- check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release -p jani
```

If the claims check reports a stale register because you changed Markdown, run `cargo run -p jani-claims -- sync` and then `-- check` again. `scan` only prints findings; it does not update `claims/`. `strict` additionally fails on open deterministic findings.

## Module map

| Path | Responsibility |
| --- | --- |
| `backend/core/` | Configuration and settings, policy, identity, data classification, journal and shared contracts |
| `backend/store/` | Durable transactional state and the attention store |
| `backend/server/` | The `jani` binary, agent loop, tool implementations, approvals and CLI |
| `backend/mirror/` | Deterministic retrospective critique |
| `backend/mcp/` | Signed MCP delegation and response-receipt gateway |
| `backend/native/` | Twelve governed native tools |
| `contract/` | Protocol definitions |
| `tools/jani-check/` | Architecture and workspace gate |
| `tools/jani-claims/` | Documentation/source claim auditing |
| `synthetic/` | Deterministic fault-corpus generation, verification and readiness evaluation |
| `docs/` | Normative contracts, schemas and subsystem guides |
| `claims/` | Generated claims register and open findings |

When adding a CLI flag, update the parser in `backend/core/src/config.rs`, the help text, the precedence rules, negative tests and both [docs/CLI_REFERENCE.md](docs/CLI_REFERENCE.md) and [docs/CONFIGURATION_REFERENCE.md](docs/CONFIGURATION_REFERENCE.md). A new settings field must stay inside the strict schema: unknown fields fail closed, credentials are environment-variable references only, and nothing in settings may grant write or command authority.

## Security-sensitive changes

- Authorization, egress, sandbox, replay, audit and credential-handling changes require explicit negative tests. Demonstrate the old behavior failing the new test.
- Never widen a boundary by default. If a capability is unavailable, add refusal behavior and a test for it rather than a weaker fallback.
- Do not add a path that lets an agent create, stage, approve or submit a human-only commit.
- Do not commit API keys, private keys, customer records, binary corpus shards, contaminated logs or generated runtime state. Only `.jani/settings.json` is unignored; journals, approvals and ledgers stay untracked.
- Changes to privilege, policy, the parser, remote receipts, persistence and native operations need independent review by someone other than the author.

## Documentation contract

- Every Markdown file with frontmatter must be registered in [docs/documentation_manifest.json](docs/documentation_manifest.json). `python tools/doccheck.py` enforces the manifest, required frontmatter, unique `JANI-*` IDs, valid statuses, review dates and source/test path existence.
- Use the status vocabulary honestly: `implemented` requires linked tests, `implemented-partial` requires stated limitations, and `proposed` means design only.
- Update the affected documents, status and limitations whenever externally visible behavior changes.
- Source-backed assertions can carry `<!-- jani-claim: ... -->` markers. `cargo run -p jani-claims -- check` verifies the committed register. AI-authored review candidates never count as verified facts.
- Do not present synthetic generation, a plan, an alert or a model assessment as executed evidence.

## Review and licensing

Keep commits focused, explain why the change is needed, and include the commands and platform you actually ran. Contributions are accepted under the repository's [MIT License](LICENSE).

## Maintenance

Keep this document aligned with implemented code and record uncertainties instead of inventing support.
