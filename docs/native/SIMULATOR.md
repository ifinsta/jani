---
id: JANI-NAT-012
title: "Deterministic simulator"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/plans.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Deterministic simulator

## Supported operations
run [SEED]

## Effect and authority
All native operations are task/tenant scoped and audited by the existing Rust policy bridge or model broker; outputs cannot grant additional rights.

## Behavior, caveats and faults
- run executes a seed-controlled simulated outcome model against dependency-ready plan nodes.
- Outputs are explicitly marked as simulated: actualExecutions must remain zero and no workspace, network, provider or external service effects occur.
- Simulator results may inform risk prioritization, but cannot verify task completion or unlock human-only planner verification.
- A changed plan or seed changes the simulation context; store the exact seed and plan hash for reproducibility.
- Statistical success over many simulations does not validate code paths or substitute for executable failure injection.
- Fault cases: extreme seed, empty plan, dependency changes, deterministic repetition, simulated failure, forged completion and unbounded node count.

## Acceptance
Exercise each fault against a real disposable Jani process and collect independent observed outcomes. Pure scenario generation never counts as passed testing.
