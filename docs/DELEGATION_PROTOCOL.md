---
id: JANI-MCP-001
title: "Delegation and initiator-controlled commit"
status: implemented-partial
owner: backend/mcp/src/lib.rs
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/mcp/src/lib.rs
tests: []
---

# Delegation and initiator-controlled commit

The initiating user's verified rights are an upper bound on agent access, not a reusable copy of their session.
## Authorization protocol
1. A trusted issuer checks current user roles, entitlements, resources and purposes before issuing an expiring signed task delegation.
2. Jani intersects signed initiator identity, delegation, signed server operation contract, Jani policy and exact resource scope.
3. The MCP source independently verifies OAuth token, audience, issuer, scopes, source data rights and row/field filters.
4. Read and prepare results must carry a source-signed digest-bound classification receipt; absent or forged receipts deny disclosure.
5. A prepared change is a proposal only. The human initiator separately authenticates, reviews exact payload and row versions, and commits through source-side user-only controls.
## Supported and excluded
- Current gateway offers a constrained HTTPS tools/call request; not broad MCP discovery or full OAuth flow.
- Agent user_commit is unconditionally unavailable even if its delegated permission set includes an update entitlement.
- Hard revocation and immutable centrally managed trusted issuers are not yet implemented by Jani.
## Acceptance
- Reject missing roles, wrong tenant, stale grant, forged result, missing field filter, and agent-side commit.

## Change control
Source changes require review against negative tests; model assessments do not replace execution evidence.
