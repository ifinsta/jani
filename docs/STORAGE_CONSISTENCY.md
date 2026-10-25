---
id: JANI-STORE-001
title: "Storage consistency"
status: implemented-partial
owner: backend/store
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/store/src/lib.rs
tests: []
---

# Storage consistency

**Status: implemented-partial.** This contract describes existing boundaries alongside explicitly identified limitations.

Several Jani persistence domains coexist and are not one globally atomic database.
## State domains
- The attention store uses single-host SQLite transactions for snapshots plus outbox events.
- Native tools use tenant/task-scoped SQLite WAL metadata, separate from any model conversation.
- Core audit uses hash-linked append-only JSONL; approval and budget records occupy separate local JSON state.
## Consistency guarantees
- A SQLite commit proves only its own local state; it cannot prove an external MCP call, a file write or a journal append succeeded.
- Corrupt audit and approval stores cannot be silently reset as proof of success. Budget use must survive restart and stricter limits remain effective.
- Future migration steps require version identifiers, consistency checks, backup/restore tests and rollback or safe-refusal semantics.
- Backup/restore must capture a coherent snapshot, verify tenant binding and validate audit chain before new effects.
## Gaps
- Plaintext local state, no WORM external anchoring, no HA replication, no shared global transactions and no certified backup tooling.
## Acceptance
- Confirm transaction rollback and reopen, tamper detection, corrupted state refusal and budget restart limits.

## Change control
Source modifications require documentation review and negative tests. Neither passing compilation nor an AI claim alone proves runtime enforcement.
