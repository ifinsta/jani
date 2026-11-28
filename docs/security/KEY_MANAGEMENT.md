---
id: JANI-SEC-004
title: "Key and issuer management"
status: implemented-partial
owner: backend/core
version: 1.0.0
last-reviewed: 2026-11-28
source:
  - backend/core/src/crypto.rs
  - backend/core/src/authority.rs
  - backend/core/src/identity.rs
  - backend/core/src/policy.rs
  - backend/server/src/gateway.rs
  - backend/server/src/broker.rs
  - backend/server/src/plugins/credential_broker.rs
  - backend/server/src/update.rs
  - backend/server/src/update/evidence.rs
tests:
  - backend/core/src/crypto.rs
  - backend/server/tests/authority.rs
  - backend/server/src/gateway.rs
  - backend/server/src/broker.rs
  - backend/server/src/plugins/credential_broker.rs
  - backend/server/tests/update_e2e.rs
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
- The local verifier requires unambiguous signer IDs and canonical Ed25519 public keys. Revocation denies the same configured public key across aliases in the covered trust lists. For retirement, keep the old public-key SHA-256 in the deny list even after removing that key from trust roots. The fingerprint hashes the raw 32 public-key bytes. Each trust list still authorizes its own signer roles. [Ed25519 verification API](https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/struct.VerifyingKey.html)
- The updater applies the configured revoked signer IDs and public-key fingerprints to fetched and retained release manifests. Retaining a manifest does not keep its signer trusted after a key is removed or revoked.
- A loaded assertion or policy can expire during a session. The agent, model transport, command broker and sidecar credential broker check the configured deadline at their action boundaries. The agent withholds late model output while recording reported usage on valid completed replies. An action already underway may still finish and require reconciliation.
- Managed distribution, live revocation service, and cross-host handling of the local nonce cache remain release work. Revocation changes take effect on newly loaded configuration. Deployments require an independently trusted clock. Existing policies without an expiry remain loadable. Operator delivery finalization and upstream connector effects require their own boundary evidence.
- Test stolen signing key, mis-scoped audience, expired identity, algorithm confusion and trust-root substitution before enterprise deployment.

## Review rule
Track this document against the cited implementation, create runtime receipts for unverified guarantees, and refuse to treat proposals as implementation.
