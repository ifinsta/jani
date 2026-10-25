---
id: JANI-TEST-003
title: "Independent test oracles"
status: proposed
owner: synthetic
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - synthetic/readiness.py
tests: []
---

# Independent test oracles

## Oracle hierarchy
- A synthetic expected-decision label is a hypothesis, not verification. A test oracle must observe Jani effects and trust-boundary states independently.
- Permission oracle reads granted identity, signed policy and exact resource; effect oracle observes filesystem/source side effect; audit oracle verifies chain and correlation.
- MCP oracle checks separately signed server receipt against source state and invocation digest; data oracle catches secret egress and tenant mixing.
## Requirements
- Store expected and observed separately, including uncertainty, skip reason, reconciliation and platform confinement.
- Detect false positive, false negative, nondeterministic replay and harness oracle bugs with positive and negative controls.
- Any unexplainable discrepancy fails closed pending human review.
## Acceptance
- Demonstrate that changing only a result digest, expiry or principal flips an authorized result to refusal in tests.

## Review
Reconcile source-mapped hooks with actual executable adapters and signed independent test evidence.
