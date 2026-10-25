---
id: JANI-NAT-004
title: "Context pruning"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/memory.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Context pruning

## Supported operations
preview, run

## Effect and scope
Every operation passes the governed native bridge or broker under tenant/task identity, policy and audit. A model invocation cannot widen granted rights.

## Contract and current limits
- preview must describe disposable metadata deletion without mutating state.
- run removes only ephemeral context/reference metadata; pinned references, source, plans, audit, approvals and evidence must survive.
- A model cannot use pruning to suppress logs, security decisions, earlier denied effects or retention obligations.
- Current pruner is not a general data erasure or legal-hold-aware retention system.
- Report before/after counts and protected-record skips as observable evidence.
- Fault cases: prune while pinned, empty store, concurrent context assemble, corrupt rows, tenant crossover and repeated idempotent prune.

## Verification
Test positive behavior, explicit refusal and each named fault through a real Jani execution. Synthetic input generation does not establish runtime pass status.
