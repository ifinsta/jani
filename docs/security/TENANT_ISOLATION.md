---
id: JANI-SEC-005
title: "Tenant isolation"
status: implemented-partial
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/native/src/state.rs
tests: []
---

# Tenant isolation

## Isolation model
- Every native record is scoped to a tenant/task boundary. MCP grants bind tenant, initiator, agent, resource and classification ceiling.
- Process host and state directory are still one trusted machine; local plaintext files do not provide strict cryptographic tenant isolation.
## Required invariants
- Canonicalize tenant and task identifiers before touching storage; do not allow path traversal or tenant-key collision.
- A cross-tenant query returns no data even when resource names and task IDs collide.
- An agent cannot select a new tenant merely by editing prompt text or signed metadata fields.
- Execution receipts and audit records must preserve tenant correlation through failures and retries.
## Gaps
- Independent encrypted tenant storage, hard isolation for malicious local operators and multi-host tenant partitioning remain future controls.
## Acceptance
- Test same task ID in two tenants, stale delegation after tenant switch, wrong-tenant storage lookups and result receipt laundering.

## Review rule
Track this document against the cited implementation, create runtime receipts for unverified guarantees, and refuse to treat proposals as implementation.
