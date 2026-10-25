---
id: JANI-NAT-007
title: "Impact estimation"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/repo.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Impact estimation

## Supported operations
analyze PATH

## Effect and authority
All native operations are task/tenant scoped and audited by the existing Rust policy bridge or model broker; outputs cannot grant additional rights.

## Behavior, caveats and faults
- Analyze uses indexed path and extracted symbol relationships to approximate reverse dependencies and related tests.
- Findings are a heuristic impact set, not a complete Rust compiler dependency graph, proof of affected behavior or permission grant.
- Each reported path should be anchored to its current metadata hash so stale index state is visible.
- Changes involving generated files, dynamic resolution, macros, multi-language dependencies or build scripts may escape heuristic discovery.
- Use impact to select review scope; all actual reads, writes and tests still pass their separate Rust capability checks.
- Fault cases: missing path, renamed module, macro dependence, false-negative test selection, symlink, empty index and scope crossover.

## Acceptance
Exercise each fault against a real disposable Jani process and collect independent observed outcomes. Pure scenario generation never counts as passed testing.
