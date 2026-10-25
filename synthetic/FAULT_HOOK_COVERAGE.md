---
id: JANI-TEST-001
title: "Fault-hook coverage"
status: implemented-partial
owner: synthetic
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - synthetic/catalog.py
tests: []
---

# Fault-hook coverage

## Catalog versus execution
- The synthetic generator enumerates 23 domains, 371 named failure hooks, 32 mutation variants and 17 profiles, plus composite faults; the separate native catalog adds 240 hooks.
- Catalogued hooks are scenarios, not proof that a corresponding injection adapter executed the actual Jani binary.
## Coverage requirements
- Each hook must map to a current source symbol/module, a controllable fault trigger, a real precondition, an expected invariant, observed result and independent evidence.
- Track unmapped, unsupported, unexecuted, executed-pass, executed-fail and quarantined hooks separately. Never report skipped as passing.
- Use weighted sampling for critical trust boundaries; do not substitute hundreds of similar harmless cases for one source-level security fault.
## Critical categories
- Permission escalation, tenant crossover, MCP forged proof, ambiguous effect replay, host process escape, audit corruption, data egress, quotas and lifecycle races.
## Exit gate
- Release readiness requires real execution receipts for mandatory hooks and investigation of every critical failure.

## Review
Reconcile source-mapped hooks with actual executable adapters and signed independent test evidence.
