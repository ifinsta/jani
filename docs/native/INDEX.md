---
id: JANI-NAT-001
title: "Index metadata"
status: implemented-partial
owner: backend/native
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/repo.rs
tests:
  - backend/server/tests/native_tools.rs
---

# Index metadata

## Supported operations
build, search QUERY

## Effect and scope
Every operation passes the governed native bridge or broker under tenant/task identity, policy and audit. A model invocation cannot widen granted rights.

## Contract and current limits
- build hashes eligible safe files and extracts Rust-like symbols, saving path/hash/metadata, not file bodies.
- search uses bounded metadata scanning; it is not semantic embedding, FTS5 or compiler-resolved dependency tracking.
- The scanner limits eligibility to 15,000 files, 1 MiB each, and excludes protected names and symlinks.
- Every rebuild still enumerates files and checks hashes; there is no file watcher.
- Index state is scoped by tenant and task and cannot grant permission to read raw source outside the broker.
- Fault cases: changed source after index, path collision, mixed tenant keys, symlink substitution, oversized file and long path.

## Verification
Test positive behavior, explicit refusal and each named fault through a real Jani execution. Synthetic input generation does not establish runtime pass status.
