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

## Contribution principles
- Maintain Jani's independent Rust runtime, default-deny policy and human-only commit boundary.
- Change proposals must identify expected effects, trust boundaries, source files, failure modes and observable acceptance criteria before code changes.
## Development checks
1. Run cargo fmt --all -- --check, cargo clippy --workspace --all-targets -- -D warnings, cargo test --workspace --all-targets and cargo run -p jani-check.
2. Run python tools/doccheck.py and cargo run -p jani-claims -- scan before requesting review.
3. Update source-backed documentation and the coverage manifest whenever adding an externally visible feature or guarantee.
4. Document implementation status accurately; do not mark synthetic generation or model-inferred behavior as passed tests.
## Security-sensitive changes
- Require explicit negative tests for authorization, egress, sandbox, external effect replay and audit changes.
- Never commit API keys, user data, trust private keys, binary test corpus shards or contaminated logs.
## Review
- Independently review changes to privileges, policy, parser, remote receipts, persistence and native operations.

## Maintenance
Keep this document aligned with implemented code and record uncertainties instead of inventing support.
