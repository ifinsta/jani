---
id: JANI-TEST-004
title: "Load, soak and degradation testing"
status: proposed
owner: synthetic
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - synthetic/native_tools.py
tests: []
---

# Load, soak and degradation testing

## Targets
- Measure bounded memory, CPU, latency, WAL growth, file descriptors, token/budget usage, index scale, alert throughput and journal append behavior.
## Campaign design
- Use small deterministic CI smoke tests, larger scheduled execution suites and controlled long-duration soak runs.
- Vary parallel tasks, disk pressure, corrupt records, stalled model endpoint, large files, extended UTF-8, dependency cycles and state reopening.
- Record p50/p95/p99 latency, throughput, peak RSS, failed operations, queue lag and cross-tenant errors with source commit and hardware profile.
## Safety and acceptance
- Operational degradation must not grant permission or skip logging. Any security-critical invariant failure blocks release regardless of overall success percentage.
- Define performance SLO only after measured baselines; no benchmark figure is certified yet.

## Review
Reconcile source-mapped hooks with actual executable adapters and signed independent test evidence.
