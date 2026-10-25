---
id: JANI-NAT-002
title: "Context selection"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/memory.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Context selection

## Supported operations
pin PATH, assemble QUERY [TOKENS], show

## Effect and scope
Every operation passes the governed native bridge or broker under tenant/task identity, policy and audit. A model invocation cannot widen granted rights.

## Contract and current limits
- pin stores an allowed metadata reference for later context selection; it does not pin raw secret-bearing file bodies.
- assemble produces bounded metadata references under an estimated token budget, prioritizing pinned and relevant records.
- show displays context references in scoped native state. Neither output is a model tool instruction or proof of source correctness.
- Sensitive file content can be obtained only through the governed workspace read path and egress controls.
- Current source classification does not fully inherit into index/context metadata; do not treat an assembled context as cleared for external egress.
- Fault cases: zero budget, stale hash, duplicate pin, giant query, mixed tenant, encrypted path and invalid UTF-8.

## Verification
Test positive behavior, explicit refusal and each named fault through a real Jani execution. Synthetic input generation does not establish runtime pass status.
