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

## Supported security claims
- Jani uses default-deny policy, scoped capabilities, source classification and a mediated broker. These are controls, not a guarantee that arbitrary code execution is safe.
- A signed policy does not replace source-side authorization or OS isolation. Windows Job Objects cannot provide the same network/filesystem isolation as Linux bubblewrap.
## Responsible handling
- Do not submit production keys, customer records, private signing keys or exploit payloads containing secrets into public issues.
- Security concerns should be reported privately to the maintainers through an available repository security-reporting channel; do not assume public issue tracking is confidential.
- For an incident, suspend external side effects, isolate the affected execution environment, preserve immutable copies of logs and rotate exposed credentials using the owning provider.
## Release posture
- Treat this repository as a supervised prototype, not an enterprise-certified autonomous operator. Review the threat model and readiness gates before enabling effects.
## Tests
- Validate default-deny writes, signed-policy rejection, hostile tool output, sandbox refusal, audit tampering and prohibited MCP commits.

## Review rule
Track this document against the cited implementation, create runtime receipts for unverified guarantees, and refuse to treat proposals as implementation.
