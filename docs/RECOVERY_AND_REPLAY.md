---
id: JANI-OPS-001
title: "Recovery and replay"
status: implemented-partial
owner: backend/server/src/agent.rs
version: 1.0.0
last-reviewed: 2026-11-28
source:
  - backend/server/src/agent.rs
  - backend/server/src/dispatch.rs
  - backend/core/src/journal.rs
  - backend/core/src/recovery.rs
  - backend/server/src/deliver/completion.rs
  - backend/server/src/deliver/queued_commit.rs
tests:
  - backend/core/src/recovery.rs
  - backend/server/tests/cli.rs
  - backend/server/tests/dispatch.rs
  - backend/server/tests/work.rs
---

# Recovery and replay

`jani recovery status` is a read-only inventory. It checks the entire journal hash chain and lists recorded completions, recorded failures, and unfinished runs that require reconciliation. Recovery of an earlier effect requires evidence from the system that performed it.

Journal writers verify the existing chain before opening it for append. An open writer verifies new records from its last trusted digest and refuses a detected truncation. Startup, audit verification and recovery require a final newline. Concurrent writers require working filesystem locks. Detecting rollback to an earlier complete chain requires an independently held cutoff. Protect the journal from writes outside the cooperating writer protocol.

The CLI and interactive run inspection commands verify the complete chain and select the requested run from the same locked read.

For recovery after loss of the local state directory, use the [cold backup and disaster recovery runbook](BACKUP_RESTORE.md).

Queued local delivery supports recovery after the worker journals the finalization intent. The worker verifies the saved intent and current claim, compares the live branch with the recorded parent and commit, and restores the reports and queue result. Windows and Linux integration tests exercise recovery on both sides of the branch update. Independent release execution is still required. [Durable work](WORK.md)

## Webhook and schedule delivery

Each live intake delivery has one task assignment. The local task store keys the delivery by tenant, source and delivery ID, and binds it to the event, parsed work contract and workspace. The first successful invocation returns a JSON `answer` and `turnStatus`. A matching retry returns the original turn status and sets `deduplicated` to `true`. `waiting_approval`, `waiting_input` and `paused` describe work that can continue through the normal task resume command. Use a fresh task ID for a new live delivery. Keep all workers that can ingest the same event on the same durable task store.

Stop all old dispatch workers before enabling this version on a shared store. Earlier binaries do not consult the new claim table. A rollback requires reconciling the newer delivery IDs before restarting old workers. If startup reports `delivery was released`, the agent turn did not start and the same delivery can be submitted again after fixing the startup error.

An interrupted claim and a legacy delivery record require operator reconciliation before further work. Identify the task and delivery from the command diagnostic and check the audit journal and the source system for effects. A crash before the first run event can leave a claim with no matching journal entry. An earlier dry-run marker alone cannot establish the safety of replay. There is no automated claim reset or cross-host reconciliation workflow. [Command-line reference](CLI_REFERENCE.md)

## Required recovery protocol
1. Before any effect, persist a task/step identifier, operation digest, policy revision, identity, resource and optional source-issued idempotency key.
2. After executing, capture a verified result receipt; if process death falls between effect and result, mark the outcome unknown.
3. Query an authoritative source before retrying ambiguous remote operations. Never interpret a model assertion, HTTP timeout or synthetic fixture as evidence of nonexecution.
4. On restart, verify worktree hashes, renewed authorization, still-valid delegation, spent budget, sandbox availability and audit integrity.
5. Fence stale workers with future lease generation tokens; only one authorized owner may commit a step.
6. Compensate only when the external system supports a separately authorized compensating operation. Do not invent payment rollback.
## Cooperative cancellation boundary (source implemented, unverified)

The interactive terminal uses an atomic cancellation token. Ctrl+C requests cancellation while keeping the session open. The agent checks before model/tool work and subsequent tool calls, and records `cancelled_by_operator` in its journal. It cannot forcibly stop an already-running command or blocked model request. Earlier external effects remain subject to reconciliation.

## Source-level recovery inventory

`jani recovery status` emits `jani.recovery-status/v1` with tenant/task/run IDs, last recorded run outcome, event counts and denials. Every run has `safeToAutoReplay: false`. A crash between the actual effect and its journal outcome can remain ambiguous even if the hash chain validates; only independent source reconciliation can resolve that. Three unit tests cover abandoned runs, recorded model completions and tampered journals.

## Evidence for independent review

`jani evidence RUN_ID` produces a verified audit-only run receipt without executing models or tools. It records a digest and outcomes as observed in the journal, explicitly distinguishing these from acceptance test results. A matching CLI integration test checks both valid evidence and refusal of corrupted evidence. No autonomous resume or signed attestation is implied.

## Current gaps
- No persisted model message cursor, tool-outcome reconciler or external effect transaction exists.
- Planner checkpoint resume explicitly reports agentExecutionResumed false and externalSideEffectsReplayed false.
## Acceptance
- Inject crashes before and after side effect, expiry during downtime and competing resume; show no duplicate irreversible action.

## Change control
Source changes require review against negative tests; model assessments do not replace execution evidence.
