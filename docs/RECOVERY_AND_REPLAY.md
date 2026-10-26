---
id: JANI-OPS-001
title: "Recovery and replay"
status: proposed
owner: backend/server/src/agent.rs
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/agent.rs
tests: []
---

# Recovery and replay

**Status:** read-only recovery inventory implemented in source, autonomous resume NOT implemented or runtime-validated. `jani recovery status` first checks the entire journal hash chain, then lists recorded model completions, recorded failures and unfinished runs as `unknown_requires_reconciliation`. It never retries tools or concludes that earlier filesystem/external changes were rolled back. These new changes still require Cargo and independent runtime validation. Current native checkpoint stores and validates plan metadata but cannot replay an LLM session.
## Required recovery protocol
1. Before any effect, persist a task/step identifier, operation digest, policy revision, identity, resource and optional source-issued idempotency key.
2. After executing, capture a verified result receipt; if process death falls between effect and result, mark the outcome unknown.
3. Query an authoritative source before retrying ambiguous remote operations. Never interpret a model assertion, HTTP timeout or synthetic fixture as evidence of nonexecution.
4. On restart, verify worktree hashes, renewed authorization, still-valid delegation, spent budget, sandbox availability and audit integrity.
5. Fence stale workers with future lease generation tokens; only one authorized owner may commit a step.
6. Compensate only when the external system supports a separately authorized compensating operation. Do not invent payment rollback.
## Source-level recovery inventory

`jani recovery status` emits `jani.recovery-status/v1` with tenant/task/run IDs, last recorded run outcome, event counts and denials. Every run has `safeToAutoReplay: false`. A crash between the actual effect and its journal outcome can remain ambiguous even if the hash chain validates; only independent source reconciliation can resolve that. Three unit tests cover abandoned runs, recorded model completions and tampered journals.

## Current gaps
- No persisted model message cursor, tool-outcome reconciler or external effect transaction exists.
- Planner checkpoint resume explicitly reports agentExecutionResumed false and externalSideEffectsReplayed false.
## Acceptance
- Inject crashes before and after side effect, expiry during downtime and competing resume; show no duplicate irreversible action.

## Change control
Source changes require review against negative tests; model assessments do not replace execution evidence.
