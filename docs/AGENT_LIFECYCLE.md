---
id: JANI-AGENT-001
title: "Agent lifecycle contract"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/attention.rs
tests: []
---

# Agent lifecycle contract

**Status: implemented-partial.** This contract describes existing boundaries alongside explicitly identified limitations.

Agent attention state, execution state and task acceptance are distinct. Receiving an alert never authorizes an action or directly wakes an LLM run.
## States and transitions
- Agent states include ready, running (reserved), sleeping, paused and quarantined. Present CLI supports registration to ready, sleep and wake.
- Alert operations include send, list, claim, accept, retry and resolve. Claims have short leases and non-replayable receipts.
- Quarantined agents cannot transition to ready by ordinary wake; future administrator unquarantine requires separate authenticated control.
## State boundaries
- Tenant/recipient identity is checked at each command in enterprise mode; stale claims, incorrect recipients and conflicting idempotency keys fail.
- The attention SQLite snapshot and outbox share a transaction, but the separate global hash journal does not.
- Alert acceptance is triage evidence only, not proof a model resumed or a task was fulfilled.
## Not implemented
- No durable model conversation continuation, multi-host broker, task ownership fencing or general unattended response loop.
## Acceptance
- Test duplicate publishes, stale lease claims, corrupt store, quarantined wake and cross-tenant inbox access.

## Change control
Source modifications require documentation review and negative tests. Neither passing compilation nor an AI claim alone proves runtime enforcement.
