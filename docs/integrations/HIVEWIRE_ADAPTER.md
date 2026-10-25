---
id: JANI-INT-002
title: "Optional Hivewire adapter"
status: proposed
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/main.rs
tests: []
---

# Optional Hivewire adapter

## Design rule
- Hivewire may coordinate workers, but Jani remains an independently executable Rust agent. No Hivewire runtime package, hidden privilege bridge or delegated authorization bypass belongs in the Jani core.
## Proposed adapter surface
- A coordinator provides an explicit, immutable task contract and approved scoped worktree, then launches Jani as a normal process.
- It consumes documented stdout status, log references and exit codes; it must not parse model prose as verified task completion.
- Alerts may request attention only. They never wake a model or authorize file edits without a fresh Jani policy evaluation.
- When parallel agents share code, the coordinator owns checkout partitioning, conflict review, merge permission and task ownership.
## Prohibited coupling
- No automatic Hivewire authority inheritance, no direct coordinator write to approvals ledger, no unaudited shell or policy bypass.
## Acceptance
- Run without Hivewire installed; test task cancellation, lease timeout, stale worker, coordinator crash and mutually conflicting assignments.

## Review and compatibility
Changes to protocol, manifests, identity or model tools require compatibility and fail-closed negative tests. Do not infer production readiness from a proposal.
