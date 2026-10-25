---
id: JANI-OPS-005
title: "Deployment and rollback"
status: proposed
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/main.rs
tests: []
---

# Deployment and rollback

## Deployment profiles
- Supervised local evaluation: use disposable checkout, restricted paths, least privilege and monitored provider spending.
- Managed organization pilot: currently blocked pending issuer management, sandbox confinement, encrypted state, signed evidence and incident response.
## Proposed safe rollout
1. Pin release commit and artifact digest and verify build provenance.
2. Provision service account with no ambient production secrets, approved state directory and exact workspace paths.
3. Enforce signed policy/identity, controlled trust roots, process isolation and provider endpoint restrictions.
4. Run health and refusal canaries before any non-read-only task.
5. Capture immutable evidence, limit budgets and operators, then progressively enable scoped low-risk effects.
## Rollback
- Suspend new work; reconcile all unknown external effects, restore verified checkpoint/state and validate journal before resuming.
- Never rollback an irreversible external transaction by copying an old SQLite file.
## Acceptance
- Test host restart, version downgrade refusal, configuration rollback and protected source preservation.

## Maintenance
Review exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
