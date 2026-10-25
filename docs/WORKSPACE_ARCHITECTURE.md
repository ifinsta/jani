# Jani workspace refactor — first implementation

The executable remains **`jani`**, with no Hivewire or other coordinator requirement.

## Packages

- `jani`: existing governed CLI, model loop, broker, sandbox, identity and policy.
- `jani-contract`: generates closed lifecycle/alert enums and transition checks from versioned JSON contracts.
- `jani-store`: SQLite transaction wrapper, attention state, schema migration ledger and transactional event outbox.
- `jani-check`: independent architecture/contract consistency gate.

The contract files are the authority for lifecycle and alert vocabulary. Future extractable security crates must not depend on the CLI or on the model client.

## Migration

The first access to the attention store imports `.jani/attention/state.json` if no SQLite snapshot exists. Migration is validated and transactional; corrupt/oversized JSON is refused, not discarded. The old JSON file remains untouched as an operator backup. After a committed SQLite snapshot, it is authoritative; do not edit both stores. The new database is `.jani/attention/state.sqlite3`.

The state and outbox event are committed together. The existing global JSONL audit journal remains separate and is **not** an atomic part of this transaction. Outbox events can be used for future audit projection; this release does not implement that projection. SQLite does not make external effects exactly-once.

## Verification

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p jani-check
cargo build --release -p jani
```

This is a **foundation** rather than a complete multi-tenant runtime. A major future phase will move per-task budgets, approvals and audit into transactional storage, then introduce real process supervision and model checkpoints. Plugins still register signed manifests only and do not execute.

### Production blockers

Cross-host consistency, tenant-isolated encryption, backed-up database verification, durable background job supervision, documented outage recovery, and externally verifiable event projections require additional implementation and testing. Keep banking actions approval-gated and disabled by default.
