---
id: JANI-NAT-006
title: "Verification selection"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/repo.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Verification selection

## Supported operations
select, run

## Effect and scope
Every operation passes the governed native bridge or broker under tenant/task identity, policy and audit. A model invocation cannot widen granted rights.

## Contract and current limits
- select uses indexed metadata to suggest relevant tests and a possible Cargo command.
- run deliberately refuses unmediated execution; actual tests must be passed through the governed workspace_command broker and sandbox.
- A test selection is advisory and cannot create a signed acceptance receipt or mark a planner node verified.
- System must distinguish test discovery, test execution and independently reviewed evidence as separate phases.
- Unsupported test ecosystems, absent tests or stale index entries must be reported explicitly rather than as success.
- Fault cases: no tests, malicious build.rs, poisoned test name, wrong repository, sandbox unavailable, skipped tests and forged success.

## Verification
Test positive behavior, explicit refusal and each named fault through a real Jani execution. Synthetic input generation does not establish runtime pass status.
