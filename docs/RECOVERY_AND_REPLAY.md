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

**Status:** a recovery design contract, not an implemented autonomous resume feature. Current native checkpoint stores and validates plan metadata but cannot replay an LLM session.
## Required recovery protocol
1. Before any effect, persist a task/step identifier, operation digest, policy revision, identity, resource and optional source-issued idempotency key.
2. After executing, capture a verified result receipt; if process death falls between effect and result, mark the outcome unknown.
3. Query an authoritative source before retrying ambiguous remote operations. Never interpret a model assertion, HTTP timeout or synthetic fixture as evidence of nonexecution.
4. On restart, verify worktree hashes, renewed authorization, still-valid delegation, spent budget, sandbox availability and audit integrity.
5. Fence stale workers with future lease generation tokens; only one authorized owner may commit a step.
6. Compensate only when the external system supports a separately authorized compensating operation. Do not invent payment rollback.
## Current gaps
- No persisted model message cursor, tool-outcome reconciler or external effect transaction exists.
- Planner checkpoint resume explicitly reports agentExecutionResumed false and externalSideEffectsReplayed false.
## Acceptance
- Inject crashes before and after side effect, expiry during downtime and competing resume; show no duplicate irreversible action.

## Change control
Source changes require review against negative tests; model assessments do not replace execution evidence.
