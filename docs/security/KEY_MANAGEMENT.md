---
id: JANI-SEC-004
title: "Key and issuer management"
status: proposed
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/crypto.rs
tests: []
---

# Key and issuer management

## Trust roots
- Distinguish a public verification key, privately held signing key, model API key, OAuth access token and ephemeral task delegation. They serve separate purposes.
- Signed identity and policy verification exist, but key material can currently be supplied by local operator CLI; that is not an immutable enterprise trust store.
## Required lifecycle
1. Generate keys outside the worktree and keep private signing material in a hardware-backed vault where possible.
2. Pin issuer, audience, tenant and key IDs in administratively controlled policy. Record issuance, expiry and permitted usage.
3. Rotate with an overlap window, revoke compromised keys centrally and reject assertions from retired roots.
4. Verify revocation and clock skew at each privileged action; fail closed during unverifiable authorization outages.
## Gaps and gates
- No managed SCIM, enterprise key rotation/revocation service or verified nonce replay cache is implemented.
- Test stolen signing key, mis-scoped audience, expired identity, algorithm confusion and trust-root substitution before enterprise deployment.

## Review rule
Track this document against the cited implementation, create runtime receipts for unverified guarantees, and refuse to treat proposals as implementation.
