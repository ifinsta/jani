---
id: JANI-NAT-003
title: "Planner DAG"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/plans.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Planner DAG

## Supported operations
create PLAN.json, inspect, report NODE EVIDENCE, verify NODE --approve

## Effect and scope
Every operation passes the governed native bridge or broker under tenant/task identity, policy and audit. A model invocation cannot widen granted rights.

## Contract and current limits
- create validates a bounded directed acyclic task graph, explicit acceptance fields and dependency references; cycles refuse.
- inspect returns recorded plan stages; report associates a node with evidence instead of declaring completion.
- verify requires an authenticated signed human identity with explicit --approve on the CLI; model native_tool cannot call it.
- A reported evidence reference is not a substitute for independent validation. Human CLI attestation is not certification.
- A downstream node is not ready before verified dependencies, and a changed plan invalidates old checkpoint assumptions.
- Fault cases: cycle, missing dependency, duplicate node ID, stale node verification, forged operator, evidence omission and plan mutation.

## Verification
Test positive behavior, explicit refusal and each named fault through a real Jani execution. Synthetic input generation does not establish runtime pass status.
