---
id: JANI-SEC-009
title: "Sentinel client monitoring and intrusion protection contract"
status: proposed
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/attention.rs
  - backend/core/src/policy.rs
tests: []
---

# Sentinel client monitoring and intrusion protection contract

**Implementation status: proposed.** Jani is not presently a fleet monitoring or endpoint protection service. Its current attention inbox is local and cannot operate as a multi-host ingestion bus.

## Product boundary

Sentinel proposes one consent-based, low-overhead, independently restartable probe per supported host. Observer covers health and log collection; Shield Detect covers read-only security events; an optional separately privileged enforcer may be designed later. Jani's existing policy engine may investigate incidents through authenticated, scoped reads only. Alerts or model-produced text are never authorization.

## Mandatory device and service obligations

1. Enroll each host to one tenant and device identity with independently authenticated cryptographic credentials. Hostname, IP, OS login name or customer input is not proof of identity.
2. Build a reconciled inventory from OS service manager, processes, configured applications and supported containers. Every discovered service has a per-source log coverage record and a last-successful-read timestamp.
3. Aggregate structured journal/event records, configured files and authorized application/container streams where their adapter is supported. Classify unsupported sources and access-denied sources accurately. Do not claim every possible application log exists or is accessible.
4. Normalize all supported metrics with pinned semantic names, types, UCUM units, clock and resource context. Reject invalid or unbounded labels. Preserve correct missing/stale/unknown distinctions.
5. Keep bounded offline retention, durable per-source cursors, orderly replay, backpressure and explicit data-loss signals. Collection must never block business applications.
6. Do not collect passwords, API credentials, keystrokes, screenshots, unrestricted browser history or unrelated files. Privacy consent, configured retention and sensitive data classification remain mandatory.

## Protected transmission and proprietary implementation

The exact Jani Secure Machine Transport (JSMT) wire protocol and runtime are intended for a **private, access-controlled repository**, not this public source tree. Obscurity is not a cryptographic control. Transport must enforce TLS 1.3, verified server identity, authenticated registered client devices, fail-closed protocol downgrade and tenant/device authorization, authenticated ACK and bounded replay protection. Internal onward hops must protect data in transit too.

No default shell access or general remote command execution is permitted. A future local enforcer requires signed specific host policy, independent authorization, time-limited permissions, safe rollback and break-glass before customer deployment.

## Fault tolerance and service continuity

The probe remains useful without Jani's AI model or a central connection. Its watchdog, bounded local spool, rate controls, failure isolation, rotation handling and expiry/revocation checks must be independently tested. At-least-once transport plus idempotent commit are acceptable; exactly-once analytics and end-to-end log completeness must never be asserted without evidence.

## Critical risk and compliance boundaries

Threats include log injection, compromised host keys, untrusted remote operator, tenant crossover, service source blind spots, false intrusion blocks, data exfiltration, installer tampering, unauthorized monitoring and audit forgery. Document applicable privacy duties, data processing contracts, source retention, software supply chain, vulnerability response and key rotation. Compliance must be measured by executed tests and operational receipts, not prose.

## Release gate

No public claims of intrusion prevention, all-services coverage, production-certified security, Rust probe availability or multi-host Jani orchestration until those features are implemented and independently verified in relevant OS environments.

See [Sentinel security and service acceptance gates](SENTINEL_RELEASE_GATES.md).
