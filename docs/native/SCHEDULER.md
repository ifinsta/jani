---
id: JANI-NAT-008
title: "Dependency readiness scheduler"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/plans.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Dependency readiness scheduler

## Supported operations
next

## Effect and authority
All native operations are task/tenant scoped and audited by the existing Rust policy bridge or model broker; outputs cannot grant additional rights.

## Behavior, caveats and faults
- next returns plan nodes whose prerequisite stages satisfy the persisted dependency readiness rules.
- Selection is not a lease, execution ownership, right to call tools or proof that a task will finish.
- The scheduler never launches processes or agents and cannot supersede a coordinator's worktree ownership policy.
- A future durable scheduler must introduce fencing tokens, fairness, starvation controls, cancellation, time budgets and restart-safe assignment.
- Prefer deterministic ordering of ready nodes and explicit blocked reasons over invisible heuristic task hopping.
- Fault cases: cycle, missing node, revised prerequisite, simultaneous callers, idle plan, pause/quarantine and stale status.

## Acceptance
Exercise each fault against a real disposable Jani process and collect independent observed outcomes. Pure scenario generation never counts as passed testing.
