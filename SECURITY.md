---
id: JANI-SEC-003
title: "Security policy"
status: implemented-partial
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/policy.rs
tests: []
---

# Security policy

Jani is a local coding agent with default-deny effects. Its security value comes from refusing writes, commands and egress that were not explicitly authorized, and from journaling every mediated decision. Those are controls, not a warranty. This page tells you how to report a weakness, what is in scope, and where the guarantees stop.

## Reporting a vulnerability

Report privately. Do **not** open a public issue for a suspected vulnerability.

- Use this repository's private vulnerability reporting / GitHub Security Advisories channel. If that channel is unavailable, contact a maintainer directly and ask for a private route.
- Include the commit SHA or version, platform, relevant flags and settings, a minimal reproduction, the observed behavior, the expected behavior and the impact you believe it has.
- Use synthetic repositories and synthetic credentials. Never send live API keys, customer data, private signing keys or payloads containing real secrets.
- Do not test against shared systems, third-party infrastructure or repositories you do not own.

Maintainers will acknowledge, reproduce and coordinate disclosure with you. There is no guaranteed response window and no bug bounty program. Please allow time for a fix before publishing details.

## In scope

A report is in scope when it crosses a documented boundary without already owning the host:

- Escaping `--write-path` scopes, writing protected or credential paths, or bypassing symlink/traversal checks.
- Running a command outside the allowlist, obtaining a shell, or smuggling arguments past command validation.
- Reading or transmitting protected credentials, or leaking the provider key into a child process, journal, diagnostic or model request.
- Bypassing egress classification, redaction or the provider/region/purpose allowlists.
- Forging, replaying or widening policy, identity, work-contract, delegation or approval evidence.
- Tampering with the audit journal or budget ledger, or causing a successful-looking audit record for a failed effect.
- Causing a sandbox to fall back silently instead of refusing an unsatisfiable isolation profile.
- Making an agent invoke a user-only MCP commit tool, or accepting a delegated result without valid receipts.
- Using `.jani/settings.json`, `--settings` or `JANI_SETTINGS` to gain write, command or credential authority beyond documented model selection and limits.

## Out of scope

- Attacks that require an already-compromised host, root/administrator, or control of the provider.
- Prompt injection that changes model suggestions but does not cross a documented Rust boundary. Model text is explicitly not authority; a bypass that turns it into authority **is** in scope.
- Missing enterprise capabilities already listed in this page, [VALIDATION.md](VALIDATION.md) or the [implementation review](docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md).
- Denial of service against your own machine, or resource exhaustion you requested by running untrusted project code.
- Secrets intentionally placed in test fixtures or example files.

## What Jani promises

| Control | Promise |
| --- | --- |
| Default deny | No write or command execution unless explicitly enabled with scoped flags. |
| Model separation | Model output, skill text and tool responses cannot grant themselves authority. |
| Narrowing only | Work contracts and enterprise policy can narrow operator scope; nothing below the operator can widen it. |
| Fail-closed data | Secret-classified content is not sent to a provider; empty allowlists deny and `"*"` is the only wildcard. |
| Honest sandboxing | Selection has no silent fallback; an unsatisfied isolation profile is an error. |
| Human-only commit | Agent invocation of user-only commit tools is always blocked. |
| Settings scope | Settings select models and limits only; inline credentials are rejected and a profile reads only its named environment variable. |
| Auditability | Each mediated decision is journaled before the effect; the journal is hash-chained and verifies on read. |

Executed negative tests for these controls are listed in [VALIDATION.md](VALIDATION.md).

## What Jani does not promise

- **OS isolation on every platform.** bubblewrap is the only backend providing filesystem and network confinement, and only on supporting Linux hosts. The Windows Job Object bounds a process tree but is not an OS sandbox. macOS has no backend.
- **Safety of allowlisted code.** `cargo test`, `pytest`, `go test`, `npm test` and similar commands run project code and build scripts. Approval of the program is not approval of the code it executes.
- **Enterprise readiness.** There is no IdP/SCIM, policy administration, key rotation/revocation, signed execution receipt, nonce replay cache, encrypted or tenant-partitioned local state, or portable sandbox.
- **Plugin execution safety.** Plugins are verified and catalogued but not executable in this build.
- **Provider behavior.** Concurrency, billing, model quality and provider-side data handling are outside Jani's boundary.
- **Protection from a racing attacker.** Path checks can lose a race against a process replacing files concurrently; use OS ACLs or a sandbox for hostile local users.
- **Reconciled spend.** Budgets bound what Jani attempts; the provider invoice is authoritative.

## Operator checklist before enabling effects

1. Read [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) and [docs/AUTHORIZATION_MATRIX.md](docs/AUTHORIZATION_MATRIX.md).
2. Run in a disposable checkout or container with restricted secrets and no access to production networks or data.
3. Grant the narrowest `--write-path` set and the fewest `--exec-program` entries that still complete the task.
4. Keep signing private keys out of the repository, the worktree, child environments and diagnostics; publish only verification keys.
5. Keep API keys in environment variables referenced by `apiKeyEnv`; never commit a literal key or paste one into settings.
6. Run `jani doctor` and `jani sandbox doctor` and read the reported support honestly. A recognized backend is not a certification.
7. Protect `--state-dir`: journals, approvals and budget ledgers are plaintext JSON.
8. Have a rollback and credential-rotation plan before the first external effect.

## If an incident occurs

Suspend external side effects and stop further runs. Isolate the execution environment without destroying evidence. Preserve immutable copies of the journal, approvals, budget ledger, provider requests and relevant logs. Verify the audit chain and identify every effect that was actually attempted. Rotate exposed credentials with the owning provider, then report privately using the channel above. See [docs/security/INCIDENT_RESPONSE.md](docs/security/INCIDENT_RESPONSE.md) and [docs/security/KEY_MANAGEMENT.md](docs/security/KEY_MANAGEMENT.md) for the detailed runbooks.

## Review rule

Track this document against the cited implementation, create runtime receipts for unverified guarantees, and refuse to treat proposals or model assessments as implementation. Release conditions are in [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md).
