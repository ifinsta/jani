---
id: JANI-TEST-002
title: "Fault-injection execution adapters"
status: implemented-partial
owner: synthetic
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - synthetic/runner.py
tests: []
---

# Fault-injection execution adapters

## Adapter contract
- Inputs: synthetic case ID, reproducible seed, hook, mutation, operating profile, expected invariant and isolated test environment.
- Action: materialize permitted fixture, inject a measurable failure, launch actual Jani executable with bounded scope and capture exact observed effects.
- Outputs: execution level, program exit, stdout/stderr hashes, audit verification, platform, artifact hash and whether an independently measurable oracle passed.
## Existing implementation boundary
- The basic runner supports only a small subset of safe CLI probes; unsupported hooks are explicitly skipped.
- Generation and SHA256 verification are L0 integrity, not runtime fault execution.
## Safety
- Run malicious input only in disposable isolated checkouts, with no production keys, network exposure or customer records.
## Development requirement
- Expand hooks incrementally by subsystem with narrow adapters and independent replay tests; gate releases on critical coverage rather than bulk count.

## Review
Reconcile source-mapped hooks with actual executable adapters and signed independent test evidence.
