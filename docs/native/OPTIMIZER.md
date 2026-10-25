---
id: JANI-NAT-011
title: "Workflow optimizer"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/memory.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Workflow optimizer

## Supported operations
review

## Effect and authority
All native operations are task/tenant scoped and audited by the existing Rust policy bridge or model broker; outputs cannot grant additional rights.

## Behavior, caveats and faults
- review examines bounded recorded evidence and Mirror retrospective findings to suggest workflow improvements.
- Recommendations must identify observed events, source runs and uncertainty, not claim unobserved model reasoning or unrecorded failures.
- Optimizer does not edit source, policy, architecture contracts or active tasks; it cannot self-apply recommendations.
- A proposed change requires a separate authorized plan, tests, reviewer judgment and normal source integration procedures.
- Prefer low-cost deterministic critiques over endless self-debates, speculative refactors and unbounded prompt recursion.
- Fault cases: absent audit, contradictory evidence, tampered history, weak recommendation, long log, multi-tenant state and repeated review.

## Acceptance
Exercise each fault against a real disposable Jani process and collect independent observed outcomes. Pure scenario generation never counts as passed testing.
