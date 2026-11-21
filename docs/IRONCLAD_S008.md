# IRONCLAD S008 — authenticated promotion and synthetic coding execution

**Status:** Experimental, read-only, offline laboratory. Not an enterprise execution service, bank integration, release qualification, or signed human session handoff.

Jani's `jani_store::dispatch::promote_planning` adds a single-host, transactionally fenced transition from an **independently approved** planning offer to a running signed assignment. The caller is trusted and supplies a verification callback; the store itself does not trust model output or the Hivewire coordination board. The transition rejects missing/disabled manager, supervisor or operator membership; reused nonces; mismatched epoch/tenant/department/task/operator; wrong agent or signed expiry; and unstructured or mismatched assignment context. Promotion and nonce consumption commit in the same SQLite transaction. Running revocation is recorded as uncertain and must not be automatically retried.

The private SIWI-side laboratory runner (stacked candidate PR #4) consumes a protected, mode-0700 fixture directory. It verifies an independently signed human IdentityAssertion and SignedAssignment using Jani's native Ed25519 verification, exact WorkContract and policy bytes, and a current real Hivewire `dispatch-snapshot`. The expected assignment context comes from the approved store, protected configuration, actual Git head and worktree and trusted documents, **not** from the submitted signature itself. It uses the Jani executable with read-only tool permissions and a bounded localhost model request. The actual user-to-agent browser auth bridge is **not** yet in place. The local board has no remote authenticated identity; a trusted OS-admin fixture bootstraps managers and grants.

After Jani runs, the independent reviewer checks the native hash-chained execution journal, requires a **genuine executed repository read**, prohibits executed writes, and compares the model answer to a source-backed deterministic oracle. A comma followed by a cosmetic space is acceptable, but invented codes, unsorted results, repeated codes, extra prose or malicious instructions are rejected. Reviewer digest and task state are stored independently of Hivewire's goal acceptance. The Hivewire task is deliberately **not** marked done based only on Jani's response. There are no file-write approvals, OS sandbox, dynamic worker isolation, key revocation across nodes, financial/PII processing, reliable external effect fencing, or autonomous bank-side changes in this lab.

## Actual test evidence

The scripted OpenAI-compatible loopback model first performs `repo.read`, then attempts `repo.write` (denied), then supplies a correct synthetic answer. Additional scenarios tamper with the signed grant, WorkContract, initiating identity and nonce or make Hivewire ownership stale. The harness checks that these fail without model invocation and without false completion. The suite is a small number of **distinct end-to-end cases**, not 2,000 independent intelligent assignments.

The open-weight CPU test uses the publicly available Qwen2.5-Coder-1.5B-Instruct Q4_K_M GGUF, pinned to SHA256 `f530705d447660a4336c329981af164b471b60b974b1d808d57e8ec9fe23b239`. The runtime is llama.cpp build b12608, verified release tarball SHA256 `437a78088609f8faac6eddb8654614a0bafd761d3e9803b304c1b623074f599c`. Model and runtime files are not committed or distributed. An optional experimental localhost protocol normalizer, `tools/ironclad_openai_bridge.py`, turns an *explicit JSON request for a function already advertised by Jani* into an OpenAI-compatible `tool_calls` entry. This normalizer **never executes tools or grants rights**: Jani independently validates every action. It is not a production proxy or MCP adapter. Without native tool use, small open-weight models were correctly rejected by the source-read verifier.

All environment and signing keys used for the demo are disposable synthetic test fixtures; the private `execution.json`, oracle, receipts and tools remain off public paths. Model egress is bound to loopback and Jani's approved `SOFTWARE_DEV` purpose; `REVIEW` egress was denied in testing and was not enabled by changing policy.

## Validation

- Jani: `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets --no-fail-fast`.
- Experimental normalizer: `python3 -m unittest tools/test_ironclad_openai_bridge.py -v`.
- Private SIWI candidate: `cargo test --locked` under `examples/jani-control`.
- A reproducible Runpod-only harness exercises the real Hivewire local Git board, cryptographic grants, Jani CLI and independent reviewer using entirely synthetic CSV records. Do not run it with production databases, keys, documents or real customer records.

**Security review required before merging.** The existing role store, one-host trusted signer, model runtime and Git coordination are all local trust boundaries, not a production distributed identity-and-action service.
