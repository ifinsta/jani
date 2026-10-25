---
id: JANI-OPS-006
title: "Platform support and confinement"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/sandbox.rs
tests: []
---

# Platform support and confinement

## Current platform boundaries
| Platform | Capability | Restriction |
| --- | --- | --- |
| Linux | bubblewrap where available | Isolation-required actions refuse if backend missing |
| Windows | Job Object process tree limits | Not filesystem or network isolation for enterprise profiles |
| macOS | No first-class isolation backend | Isolation-required profiles unavailable |
## Operator verification
- Run sandbox doctor and a constrained dry run on the exact target host; platform detection alone is insufficient.
- Confirm worktree read/write mounts, network namespace, process lifetime limits and absence of provider secrets in child environments.
## Forbidden descriptions
- Do not call Windows Job Objects a complete container sandbox. Do not claim macOS parity, portable seccomp or destination-level network allowlist.
## Release gates
- Independent escape/hostile build tests, process-tree cancellation, platform-specific failure refusal and accurate confinement receipts.

## Maintenance
Review exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
