# IRONCLAD S010 — one-use SIWI session-bound dispatch authorization

**Review candidate only.** This repository adds a transactional, single-host authorization-intent store. It does not independently authenticate an end user, sign an actual assignment or start a worker.

## Trusted boundary

A financial-profile SIWI relying party has authenticated a human with a recent ACR2 step-up, a private-key JWT client and a valid HTTPS origin. Only that trusted server may generate an identity assertion using a pinned Ed25519 key and write an `IssueSession` record. The original cookie, provider token and private signing key must not be sent to a browser or to a Jani agent. The exact issuer, client ID, subject, tenant, department, task and approved operator membership are checked again inside the SQLite transaction. The signed assertion can be verified using Jani's public trust root, but this store checks only its syntax and binding: **cryptographic verification belongs at the privileged consumer**.

Each record is limited to 60 seconds and a single task epoch. A second request for the same tenant/task/epoch is refused. A privileged manager, *after* independently revalidating the exact session hash against SIWI's encrypted RP-namespaced shared store, consumes the intent once in a transaction. The manager refuses expired, already claimed or revoked intents, changed membership, changed approval, or an unexpected session reference. An operator's account suspension, a maker or supervisor membership suspension, or planning revocation invalidates any unclaimed authorization in the same native SQLite transaction. The signed assignment and real Hivewire/OS executor remain separate gates.

## Validation

Five focused Rust tests in `backend/store/src/dispatch/session_intent.rs` cover durable one-time consumption, cross-tenant/identity/scope refusal, exact signed assertion shape, concurrent claims (only one winner), and operator/maker/supervisor/approval revocations. The native regression suite must also pass `cargo fmt --all -- --check`, Clippy with `-D warnings`, the full locked workspace suite and the existing 2,000-case synthetic security/MCP matrix. The private SIWI-side tests exercise the actual financial-profile Axum extractor and a genuine TLS Redis instance. No real customer, bank or IdP credentials are used.

## Risks not resolved

- The current manager and dispatcher share a single trusted host and database; there is **no distributed linearizable lease** across separate workers and no atomic Redis/SQLite transaction.
- A read-only Redis liveness check can race with a later logout until the next check. The process itself must be isolated, and production sources must enforce their own OAuth/ACL and revocations.
- A real Accounts passkey registration/step-up ceremony, independent production operator onboarding, KMS/HSM key management, signed external MCP data receipts, worker isolation and resilient multi-host dispatch have **not** been validated by this change.
- Runpod's present container rejects unprivileged user namespaces, so no production OS sandbox or trustworthy unattended action runner is claimed.
- The existing offline lab may execute only synthetic, preapproved read-only fixture work and cannot be used as a banking authority.

**Do not merge as a production banking authorization implementation without independent threat modeling, privileged service isolation, signed runtime trust roots and external security review.**
