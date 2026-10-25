# Jani native autonomy tools v1

Jani implements the twelve tools listed below as a single Rust package,
`backend/native`. The CLI `jani native TOOL OPERATION [ARGS] --task TASK`
is mediated by signed identity (enterprise), Jani policy evaluation and its
hash-chain journal. An agent can call the same operations through the
`native_tool` function in Jani's existing tool broker. **No Hivewire dependency.**

| Tool | Operations | Core behavior |
|---|---|---|
| Index | `build`, `search QUERY` | Hash and index safe repository paths and Rust-like symbols; no file bodies persisted |
| Context | `pin PATH`, `assemble QUERY [TOKENS]`, `show` | Assemble references under an estimated metadata token budget, preserve pinned references |
| Planner | `create PLAN.json`, `inspect`, `report NODE EVIDENCE`, `verify NODE --approve` | Validate DAG/acceptance, persist task plan and stage status changes; final verification requires authenticated human CLI |
| Pruner | `preview`, `run` | Remove disposable context metadata only; never remove source, pinned records, audit or approvals |
| Checkpoint | `create`, `inspect`, `resume` | Persist/check plan digest; inspect what may resume, not a hidden LLM continuation |
| Verifier | `select`, `run` | Select likely test files and propose Cargo command; `run` explicitly denies any ungoverned execution |
| Impact | `analyze PATH` | Approximate reverse path/symbol relationships and related tests |
| Scheduler | `next` | Return dependency-ready nodes without authorizing any tool side effects |
| Workspace | `status`, `snapshot`, `rollback` | Compare indexed content hashes to source, persist metadata snapshot; `rollback` fails closed |
| Evidence | `record ID FILE`, `check ID`, `list` | Store immutable source digests and check later changes; no AI self-attestation |
| Optimizer | `review` | Use verified audit history and Mirror heuristics to recommend changes, never auto-apply |
| Simulator | `run [SEED]` | Deterministic, explicitly simulated failure/success on ready plan nodes, no effects |

A deliberately small model-facing `native_tool` function accepts
`{"tool":"index","operation":"search","args":["PolicyEngine"]}`.
It cannot call `planner verify`, `workspace rollback`, or `verifier run`.
All other operations still pass the same signed organizational policy and
work-contract checks as Jani's existing tools. A model request alone
does not grant permission.

## Example

```bash
jani native index build --repo . --task TASK-1
jani native index search PolicyEngine --repo . --task TASK-1
jani native context assemble PolicyEngine 8000 --repo . --task TASK-1
jani native planner create docs/my-task.json --repo . --task TASK-1
jani native scheduler next --repo . --task TASK-1
jani native checkpoint create --repo . --task TASK-1
jani native simulator run 42 --repo . --task TASK-1
jani native verifier select --repo . --task TASK-1
jani native optimizer review --repo . --task TASK-1
```

Plan format:

```json
{
  "nodes": [
    {"id":"inspect","deps":[],"status":"planned","priority":3,
     "acceptance":"Relevant implementation files identified"},
    {"id":"test","deps":["inspect"],"status":"planned","priority":2,
     "acceptance":"Security regression tests pass"}
  ]
}
```

For human verification, the node first requires a recorded evidence reference
and a `planner report NODE EVIDENCE` transition. Then an authenticated human
initiator may use `planner verify NODE --approve`; local unverified
identities cannot perform it. Manual approval is only an **operator
attestation**, not independently verified test evidence.

## Persistence, performance and safety

The metadata store is `.jani/native/native.sqlite3` (SQLite WAL and FULL
synchronous mode) with tenant + task isolation. Indexing is bounded to 15,000
eligible files, each capped at 1 MiB, and avoids recognized protected paths,
symlinks, executable source and secret-file names. Index entries contain paths,
content hashes and extracted symbol names—never source file bodies.

Index build hashes source files; incremental rebuilds skip unchanged
metadata. The first version still enumerates eligible files and rehashes them,
rather than using an OS-level watcher. Search is indexed metadata scanning,
not FTS5/BM25 or a full language-semantic index. Impact analysis uses
path/symbol heuristics, not a complete compiler-generated dependency graph.

Checkpointing persists a plan snapshot but **does not** resume suspended LLM
tokens or execute work after a crash. Scheduler provides a proposal, not
ownership leases. Pruner removes disposable context only. Simulated successes
and suggestions are not accepted as task completion.

Every native operation is audited through Jani's runtime. There is no
automated source editing or network egress from the native package; tools
that require effects must use the pre-existing governed broker. Release
validation must include permission-boundary, process-confinement and
cross-tenant tests.

## Verification

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
python -m unittest discover -s synthetic/tests -v
python -m synthetic.native_tools generate --output synthetic/generated/native_240k
python -m synthetic.native_tools verify --output synthetic/generated/native_240k
```

See `synthetic/tests/test_native_tools.py` for deterministic generator tests
and `backend/server/tests/native_tools.rs` for actual CLI routing tests.
Synthetic input generation never certifies Jani as production ready.

**Boundaries still to implement before mission-critical autonomy:** source
content classification inherited into index/query results; complete Rust
semantic dependency extraction; signed evidence provider attestations;
durable LLM continuation; lease-backed scheduler; cross-process atomic outbox
for native state and audit; safe patch/rollback via authorized Workspace
broker; and full fault-injection adapters for each named failure hook.
