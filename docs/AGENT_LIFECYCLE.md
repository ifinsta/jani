---
id: JANI-AGENT-001
title: "Agent lifecycle contract"
status: implemented-partial
owner: backend/server
version: 1.2.0
last-reviewed: 2026-10-26
source:
  - backend/server/src/attention.rs
  - backend/server/src/agent.rs
  - backend/server/src/repl.rs
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
## Session continuation
- `jani repl`, or bare `jani` when stdin is a terminal, keeps conversation history in process across turns under one task ID, with one run ID per turn. Authority is loaded once at session start and cannot be widened by a slash command.
- With both streams attached to a terminal, the session presents a full-screen interface; the model turn runs on a scoped thread while the interface keeps accepting edits and scroll commands. Turn progress is forwarded as events: step changes, tool start and finish with a human summary, and inspected answer and reasoning previews. The session only previews complete inspected lines, so preview and final message keep the same output governance. Presentation is the only difference: the same agent session, policy, approvals and journal calls run underneath, and redirected streams fall back to the line prompt.
- History is bounded and trimmed at user-turn boundaries; a failed turn rolls back to its pre-prompt checkpoint. No conversation state is persisted, so a restart begins fresh.

## Not implemented
- No durable model conversation continuation across process restarts, multi-host broker, task ownership fencing or general unattended response loop.
## Acceptance
- Test duplicate publishes, stale lease claims, corrupt store, quarantined wake and cross-tenant inbox access.

## Change control
Source modifications require documentation review and negative tests. Neither passing compilation nor an AI claim alone proves runtime enforcement.
