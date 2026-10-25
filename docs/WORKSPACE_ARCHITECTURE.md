# Jani — workspace architecture (modeled on ifinpay)

**Status:** implemented structural migration on PR #2; build and integration verification pending.

Jani intentionally uses the same top-level vocabulary as ifinpay: `backend/`, `contract/`, `tools/`, and `docs/`. Rust's conventional `src/` directory remains **inside each package only**. `Cargo.toml` at the root is a virtual workspace.

```text
jani/
├── Cargo.toml                  # virtual Cargo workspace
├── Cargo.lock
├── rust-toolchain.toml
├── contract/                   # closed lifecycle/alert state vocabularies
│   ├── VERSION
│   ├── lifecycle/states.json
│   └── alerts/states.json
├── backend/
│   ├── core/
│   │   ├── Cargo.toml          # jani-core
│   │   ├── build.rs            # generates typed contract transitions
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── contract.rs
│   │       ├── policy.rs
│   │       ├── identity.rs
│   │       ├── types.rs
│   │       ├── data.rs
│   │       ├── approvals.rs
│   │       ├── budget.rs
│   │       ├── journal.rs
│   │       └── ...
│   ├── mcp/                   # jani-mcp: signed governance contract verifier
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── mirror/                # jani-mirror: read-only reflection engine
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── store/
│   │   ├── Cargo.toml          # jani-store
│   │   └── src/lib.rs          # SQLite attention + transactional outbox
│   └── server/
│       ├── Cargo.toml          # jani binary package
│       ├── src/
│       │   ├── main.rs
│       │   ├── agent.rs
│       │   ├── attention.rs
│       │   ├── broker.rs
│       │   ├── gateway.rs
│       │   ├── model.rs
│       │   ├── plugins.rs
│       │   ├── sandbox.rs
│       │   └── ...
│       └── tests/
│           ├── cli.rs
│           └── e2e.rs
├── tools/
│   └── jani-check/
└── docs/
```

## Dependency boundaries

1. `jani-core` depends on no server, model orchestration, plugin executor, or coordinator.
2. `jani-store` is an independent storage primitive.
3. `jani` composes the trusted core, storage, tool broker and model runtime. It remains the only user-facing executable.
4. `jani-mirror` reads trusted audit data through `jani-core` without depending on the agent runtime, model clients or coordinator; recommendations are explicitly persisted via `jani-store`.
5. The core's build script generates Rust state enums/transitions from `contract/`; do not introduce competing local vocabularies.
6. No Hivewire or mandatory multi-agent framework dependency. A future coordinator may call the CLI using an optional adapter.

## Compatibility

Running `cargo run -- --version` at repository root still works via `default-members = ["backend/server"]`. The Rust binary remains `jani`. The existing CLI commands remain available. Core imports in the executable are compatibility re-exports from `jani-core`; new code should depend directly on shared core contracts where possible.

Existing alert state at `.jani/attention/state.json` imports into `.jani/attention/state.sqlite3` on first access, provided it is valid. The old JSON remains as an operator backup. The SQLite snapshot is authoritative after the import; do not mutate both.

The attention snapshot and outbox event commit in one SQLite transaction. The existing global JSONL audit journal, approvals and budgets remain on separate persistence and must not be presented as atomic with the attention store.

## Verification

```bash
cargo fmt --all -- --check
cargo run -p jani-check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p jani
cargo run -- --version
```

Outstanding scope: real supervised model checkpoint/resume, persistent per-task transactional governance, externally validated evidence, runnable plugin isolation, and distributed multi-tenant deployment. No production-readiness claim is made.
