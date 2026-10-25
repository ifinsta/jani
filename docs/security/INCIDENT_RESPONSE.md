---
id: JANI-SEC-007
title: "Security incident response"
status: proposed
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/journal.rs
tests: []
---

# Security incident response

## Detection
- Trigger incidents on unauthorized effects, forged receipts, cross-tenant results, failed audit verification, secret leakage or bypassed confinement.
## Containment workflow
1. Halt affected tasks and revoke their ability to invoke external effects; do not delete forensic files.
2. Preserve journal, affected state directory, model/tool request digests, provider event IDs, hashes and timeline.
3. Rotate exposed credentials through their owner; notify relevant tenant authority and incident owner.
4. Correlate effects with source authoritative records; mark unverified outcomes unknown.
5. Patch the root cause, add reproducible regression and chaos tests, obtain independent sign-off and restore restricted service.
## Current gap
- Jani has no incident state machine, paging pipeline, remote containment coordinator or automatic revocation service.
## Acceptance
- Drill a forged authorization, corrupt audit chain and interrupted MCP prepare; verify containment without duplicate side effects.

## Review rule
Track this document against the cited implementation, create runtime receipts for unverified guarantees, and refuse to treat proposals as implementation.
