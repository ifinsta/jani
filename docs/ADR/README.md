---
id: JANI-GOV-004
title: "Architecture decision records"
status: proposed
owner: project
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - docs/ARCHITECTURE.md
tests: []
---

# Architecture decision records

## Purpose
- Capture material architectural decisions separately from mutable implementation guides and avoid contradictory proposals.
## Decision template
- ID, title, date, status (proposed, accepted, superseded or rejected), context, alternatives, decision, consequences, security implications, source files, tests and migration plan.
## Decision requirements
- Make authority boundaries explicit. A decision to widen privileges, merge storage, add plugin execution or change remote effect semantics needs security review.
- Record why a simpler alternative was rejected, measurable rollout prerequisites and the event that would trigger reconsideration.
## Initial backlog
- ADR for mandatory broker mediation, optional Hivewire compatibility, separate MCP human-only commits, SQLite/JSONL audit boundaries, fail-closed sandbox selection and independent evidence.
## Governance
- Never rewrite an accepted historical ADR to hide a reversal. Add a superseding ADR and point to the implementation commit.

## Maintenance
Keep this document aligned with implemented code and record uncertainties instead of inventing support.
