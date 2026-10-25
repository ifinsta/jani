---
id: JANI-NAT-010
title: "Evidence ledger"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/memory.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Evidence ledger

## Supported operations
record ID FILE, check ID, list

## Effect and authority
All native operations are task/tenant scoped and audited by the existing Rust policy bridge or model broker; outputs cannot grant additional rights.

## Behavior, caveats and faults
- record links an evidence identifier to a source-file digest at the time it is recorded.
- check compares that digest with current source state; list displays evidence references in tenant/task scope.
- A matching file hash proves only byte equality for that file; it does not prove tests were executed, a business action succeeded or a human approved.
- Never convert model-supplied narrative into verified security evidence without trusted external corroboration.
- Existing evidence and audit records are protected from generic pruning; evidence is not a substitute for an externally signed execution receipt.
- Fault cases: duplicate ID, modified file, deleted file, forged hash, secret filename, tenant crossover and stale references.

## Acceptance
Exercise each fault against a real disposable Jani process and collect independent observed outcomes. Pure scenario generation never counts as passed testing.
