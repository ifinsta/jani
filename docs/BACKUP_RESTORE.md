---
id: JANI-OPS-008
title: "Backup and disaster recovery"
status: proposed
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/store/src/lib.rs
tests: []
---

# Backup and disaster recovery

## Protection scope
- Include attention SQLite state/outbox, native tenant/task SQLite WAL state, core JSONL journal, approval records, budget ledgers, policy artifacts and signed grant audit references.
- Do not back up external provider API keys in plain archives. Handle sensitive state according to classification and retention rules.
## Proposed procedure
1. Quiesce mutable tasks and record a checkpoint of active operations and unknown outcomes.
2. Capture a consistent SQLite backup and WAL state rather than copying changing database files blindly.
3. Snapshot each independent journal/ledger with hashes and ownership metadata; encrypt with external keys.
4. Restore into isolated verification environment, validate schema/version, tenant boundaries and full journal chain.
5. Reconcile external side effects before reopening privileged tools; never assume local restore reversed remote effects.
## Readiness tests
- Corrupt a shard, restore to older state, run disk-full failover, lose key material and interrupt backup to prove recoverability or safe refusal.
## Current gap
- No managed consistent multi-store backup pipeline or certified recovery-time objective exists.

## Maintenance
Review exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
