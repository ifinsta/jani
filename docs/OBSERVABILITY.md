---
id: JANI-OPS-007
title: "Observability and evidence"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/journal.rs
tests: []
---

# Observability and evidence

## Required signals
- Record run ID, task, tenant, actor, action, resource digest, permit/deny decision, policy revision, budget consumption, outcome and receipt IDs.
- Distinguish agent turns, tool attempts, real effects, hypothetical simulation, alerts and post-hoc Mirror critique.
## Audit boundaries
- Current hash-linked JSONL journal verifies local chain integrity but has no external WORM anchoring or signed remote receipts for every action.
- Do not log raw API keys, signed bearer tokens, secrets, unfiltered high-classification MCP data or unnecessary customer identifiers.
## Proposed metrics
- Tool deny rate by cause; external outcome-unknown count; drifted docs; platform confinement refusal; budget exhaustion; unresolved approvals; alert lease conflict; source reconciliation age.
## Alerts
- Escalate audit corruption, cross-tenant access attempts, repeated token errors and unclassified outbound data. Alert delivery is not task authority.
## Acceptance
- Verify trace correlation and log redaction through injected faults, including a full disk and truncated journal.

## Maintenance
Review exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
