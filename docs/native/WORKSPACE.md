---
id: JANI-NAT-009
title: "Workspace metadata"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/repo.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Workspace metadata

## Supported operations
status, snapshot, rollback

## Effect and authority
All native operations are task/tenant scoped and audited by the existing Rust policy bridge or model broker; outputs cannot grant additional rights.

## Behavior, caveats and faults
- status compares indexed content hashes against actual repository content and reports drift.
- snapshot records workspace metadata state; it does not create an independently verified source backup or atomic external rollback point.
- rollback deliberately refuses; source restoration requires a separately governed workspace operation, approval and verified backup.
- Never use a metadata snapshot to overwrite authorized developer edits or undo an irreversible external effect.
- Paths remain subject to protected components, symlink rules and the governing broker when a real file effect is requested.
- Fault cases: concurrent edit, missing file, path traversal, untracked file, stale index, denied rollback and changed policy.

## Acceptance
Exercise each fault against a real disposable Jani process and collect independent observed outcomes. Pure scenario generation never counts as passed testing.
