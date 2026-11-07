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
- build hashes eligible safe files and extracts bounded Rust/Python/JavaScript-like symbols and lexical import/module tokens, saving path/hash/metadata, not file bodies.
- Unchanged older index records without dependency metadata are refreshed on the next build. The tokens are hints, not type-checked import edges or an interprocedural graph.
- search uses bounded metadata scanning; it is not semantic embedding, FTS5 or compiler-resolved dependency tracking.
- The scanner limits eligibility to 15,000 files, 1 MiB each, and excludes protected names and symlinks.
- Every rebuild still enumerates files and checks hashes; there is no file watcher.
- Index state is scoped by tenant and task and cannot grant permission to read raw source outside the broker.
- Fault cases: changed source after index, path collision, mixed tenant keys, symlink substitution, oversized file and long path.

## Verification
Test positive behavior, explicit refusal and each named fault through a real Jani execution. Synthetic input generation does not establish runtime pass status.

## Cargo-resolved package graph import

The read-only `index cargo-graph RELATIVE_METADATA_JSON` operation imports a **Cargo-resolved package-level dependency graph** into an inspection result without executing repository code. First produce metadata through the existing governed command/sandbox route:

```bash
cargo metadata --locked --offline --format-version 1 > .jani/cargo-metadata.json
jani native index cargo-graph .jani/cargo-metadata.json --repo . --tenant TEAM --task TASK
```

The JSON must describe the same canonical workspace root, have Cargo metadata format version 1, complete workspace members and `resolve.nodes.deps`. The importer checks bounded size, references and duplicate IDs and emits a SHA-256 evidence digest. It **does not** prove that the metadata was generated against the current commit; that provenance must come from the governed command receipt. Cargo resolution is package-level, not compiler-resolved Rust function or callsite edges. The existing file/symbol dependency index remains heuristic.

