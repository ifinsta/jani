---
id: JANI-TEST-005
title: "Test evidence provenance"
status: proposed
owner: synthetic
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - synthetic/readiness.py
tests: []
---

# Test evidence provenance

## Required evidence envelope
- Case ID and generator digest, Jani source commit and executable hash, host OS/container image, runner identity, test command, injected hook, expected invariant, observed outcome, timestamps and output artifact digests.
- Include independent oracle method, audit journal verification, effective policy hash and reviewer decision.
## Trust and retention
- Evidence produced solely by a model cannot self-assert execution or pass status. Sign receipts with a CI identity outside the tested process.
- Preserve failed and ambiguous trials, store redacted data, make artifacts reproducible and enforce retention/access rules.
## Readiness checks
- Reject receipts with missing mandatory source hashes, mismatched runner identity, invalid signatures, incomplete execution or unsupported platform assumptions.
- Do not treat generated cases, simulated runs and a green CI build as interchangeable evidence.

## Review
Reconcile source-mapped hooks with actual executable adapters and signed independent test evidence.
