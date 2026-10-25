---
id: JANI-INT-001
title: "MCP source-server integration"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/mcp.rs
tests: []
---

# MCP source-server integration

## Interoperability profile
- Jani currently supports a constrained HTTPS MCP tools/call exchange using signed governance contracts, delegations and receipts. It is not a full MCP discovery or credential-management client.
## Server implementer checklist
1. Expose a fixed tools/call operation approved in the signed governance contract, with closed input schema and explicit read or prepare mode.
2. Independently verify bearer token issuer/audience/scopes, current source principal rights, field/row filters and exact resource binding.
3. Verify the signed Jani task delegation and intersect it with current source authorization; metadata alone is never sufficient.
4. Return a source-signed receipt binding payload hash, request hash, tenant, user, task, resource, classification, filtering and expiry.
5. Implement human-only commit on a separate authenticated endpoint; bind approved patch digest, optimistic row versions, TTL and nonce.
## Error handling
- Deny missing governance receipts, unexpected redirect, unsupported method or version and ambiguous outcome. Do not automatically repeat prepare after transport failure.
## Verification
- Create a test server with forged roles, stale receipts, partial HTTP/SSE response and mismatched digests; show fail-closed behavior.

## Review and compatibility
Changes to protocol, manifests, identity or model tools require compatibility and fail-closed negative tests. Do not infer production readiness from a proposal.
