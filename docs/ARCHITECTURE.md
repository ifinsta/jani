# Architecture and boundaries

```mermaid
flowchart TB
  Operator[Operator: worktree + flags + write scopes] --> Jani[Jani Rust process]
  Policy[Signed policy bundle] --> PDP[Policy engine]
  Identity[Signed identity assertion] --> PDP
  Contract[Work contract: restrict only] --> PDP
  Provider[OpenAI-compatible LLM endpoint] <-->|redacted, authorized requests| Gateway[Model gateway + DGK]
  PDP --> Broker[Tool broker]
  Gateway --> Broker
  Broker -->|read SKILL.md| Skills[Installed skills advisory]
  Broker -->|scoped tools| Worktree[Git worktree]
  Broker -->|allowlisted argv| Sandbox[Sandbox manager + process boundary]
  Broker -->|append-only record| Journal[Hash-chained audit journal]
  Broker -->|budget counters| Budget[Persisted budget ledger]
  Broker -->|exact-binding decisions| Approvals[Approval store]
  Plugins[Signed plugin manifests] --> Broker
```

## Scope

Jani is a single-process coding agent. It has no embedded coordination engine,
task database, message bus or merge authority. Its job is to inspect a worktree,
edit files within operator-approved scopes, run allowlisted commands, and report
what it observed. Any multi-agent coordination is external to this binary.

Within that single process it implements a trusted local kernel: signed policy,
verified identity, data-governance egress control, a mediating tool broker, a
hash-chained journal, persisted budgets, approvals and OS sandbox selection. The
proposal review in
[`docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md`](jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md)
lists what is still missing; notably there is no IdP, policy administration,
plugin executor or Windows enterprise confinement.

## Authority

Rust is the only authority inside Jani. Authority is layered; each layer can
narrow the ones above it but never widen them.

1. **Operator permission:** writes and command execution are default-denied.
   `--write-path`, `--allow-exec` and related flags are a ceiling.
2. **Signed policy:** enterprise mode requires a signed, tenant-bound
   `OrganizationPolicy`. `defaults.action` must be `deny`; rules are matched by
   action, resource, risk and effect; explicit denies win. Roles are not part of
   verified identity, so role-scoped rules are rejected at load and never match.
3. **Verified identity and contract:** a signed `IdentityAssertion` binds tenant
   and principal. A `WorkContract` can restrict capabilities, write paths, the
   sandbox profile and budgets; omitting a capability or write path grants nothing.
4. **Tool broker:** every model tool call is mapped to a typed `ActionRequest`,
   validated, policy-evaluated, approval-checked, budget-checked and journaled
   before execution. Unknown tools and malformed arguments are journaled denials.
5. **Data gateway:** only the gateway holds the provider credential. It classifies
   decoded message content, redacts secrets, builds data envelopes, authorizes
   egress, then inspects the response. Empty provider/region/purpose allowlists
   deny; `"*"` is the only wildcard.
6. **Sandbox and process boundary:** children receive no shell and no provider API
   key, and have bounded time and output. Isolation-required profiles fail closed
   when no backend can satisfy them; the Windows Job Object reports that it does
   not confine the filesystem or network.
7. **Audit:** the journal is a SHA-256 hash chain. If a record cannot be written,
   the action is denied rather than executed unaudited.

## Lifecycle

1. Parse flags and environment; load policy and identity; validate the optional
   contract; load the plugin registry if configured.
2. Discover skills. Reading a skill is a mediated `skill.read` action and does not
   change permissions.
3. Open the persisted budget ledger for the task.
4. The model requests tools. Rust validates each request and either performs it or
   refuses it, journaling the decision and any sandbox receipt ID.
5. The gateway redacts and authorizes every model request; provider failures
   (`408`, `429`, `500`, `502`, `503`, `504`) are retried a limited number of times.
6. Jani reports tool results and a final answer. Completion is whatever the
   operator or an external workflow observes; Jani does not assert coordination
   state it does not own.

The turn loop is bounded by `--max-steps` and by the policy/contract budget.

## Skills and collaboration

Skills are Markdown instructions discovered from user and repository directories.
They can describe collaboration protocols, review practices or project
conventions. Reading a skill does not change Jani's permissions.

If a workflow needs coordination — task claims, epochs, message leases, merge
gates — that logic belongs in an external coordinator or a skill-driven process
that invokes Jani as a normal CLI. Jani grants it no special authority.

## Rust workspace layout

Jani follows an ifinpay-style Rust workspace. There is **no root-level `src/` and no `crates/` directory**.

- `contract/`: versioned vocabulary and state machine source of truth.
- `backend/core/` (`jani-core`): signed identity, policy, data-governance, core types, budget, audit, approvals and generated contracts; no dependency on the model or CLI.
- `backend/store/` (`jani-store`): SQLite-backed attention state plus transaction outbox.
- `backend/server/` (`jani`): executable and composition root; model loop, broker, sandbox, skills, plugins, alert commands, and CI end-to-end tests.
- `tools/jani-check/`: architecture gates and contract checks.

`backend/<package>/src/` exists because Cargo uses `src/lib.rs` and `src/main.rs` **inside each package**. The repository root is a virtual workspace, not a Rust source crate.

The server currently contains more modules than it should long-term. Further extraction should follow demonstrated dependency boundaries, with no speculative empty packages.

See [WORKSPACE_ARCHITECTURE.md](WORKSPACE_ARCHITECTURE.md).

## Deliberate architectural decisions

- Jani uses the **OpenAI-compatible Chat Completions tool interface**, not a Python SDK or provider-specific agent framework.
- A `SKILL.md` is advisory. Every permission claim the model is told about must also be enforced in Rust.
- Coordination is deliberately out of scope. External workflows can invoke the same CLI without changing Jani's privilege model.
- Plugins are verified and catalogued, but this build refuses native dynamic
  loading and has no WASM/sidecar/MCP executor. A plugin cannot gain authority by
  being installed.
- Environment and file-system checks are defense in depth, not an OS-level
  sandbox. On Windows and macOS they are not confinement at all for isolation
  purposes; enterprise isolation profiles are refused there rather than claimed.
- State is local plaintext JSON. The journal is tamper-evident, not tamper-proof,
  and is not externally anchored.
