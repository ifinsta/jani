---
id: JANI-NAT-005
title: "Plan checkpoint"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/plans.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Plan checkpoint

## Supported operations
create, inspect, resume

## Effect and scope
Every operation passes the governed native bridge or broker under tenant/task identity, policy and audit. A model invocation cannot widen granted rights.

## Contract and current limits
- create stores a digest-bound snapshot of the native plan; inspect reports saved metadata.
- resume validates the checkpoint against current plan state but does not replay a model token stream, tool effect or suspended process.
- No automatic restart should treat plan metadata as execution authorization, approval or evidence that an external effect was not performed.
- A future recovery engine must persist tool intents/outcomes, recheck rights, use lease fencing and reconcile uncertain effects before replay.
- The current response explicitly states agentExecutionResumed false and externalSideEffectsReplayed false.
- Fault cases: changed plan, corrupted checkpoint, process crash, stale delegation, expired budget, duplicate resume and out-of-order node.

## Verification
Test positive behavior, explicit refusal and each named fault through a real Jani execution. Synthetic input generation does not establish runtime pass status.
