---
id: JANI-REL-002
title: "Sentinel security compliance and service delivery gates"
status: proposed
owner: project
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - docs/RELEASE_ACCEPTANCE.md
  - docs/THREAT_MODEL.md
  - backend/server/src/attention.rs
tests: []
---

# Sentinel security and service acceptance gates

**Mandatory planned compliance contract, not an implemented release certificate.** The open-source JSMT/1 Python reference is published at [the protocol directory](../protocol/jsmt/v1/README.md), but passing its tests is not a Rust or production certification. Jani must reject attempts to label experimental host monitoring or intrusion protection as production ready without independently observed runtime evidence.

## Threat and safety gates

| Gate | Acceptance criteria |
| --- | --- |
| Authorized enrollment | Device-bound, tenant-scoped keys; time-bounded bootstrap; signed installer; revocation and renewal tested |
| Transport protection | Actual TLS 1.3 negotiation and mutual device authentication; plaintext, downgrade, unregistered device and hostname mismatch rejected |
| Replay and persistence | Durable ACK only after commit; duplicate same-digest idempotent; changed digest or sequence gap rejected; crash after effect reconciled |
| OS access | Safe host privilege boundaries, bounded file/event access, source ACL errors reported, no arbitrary remote shell |
| Log coverage | Inventory denominator and source evidence; missing, denied, unsupported and partial sources visible; OS/container-specific live trials |
| Normalized telemetry | Named and typed metric units, resource IDs, approved labels, timestamps and explicit freshness; invalid metrics rejected |
| Outage safety | Agent crash or remote outage never takes down applications; bounded queue, circuit breakers, quota and backoff observed |
| Data governance | Sensitive-data filtering, privacy consent, configured retention/deletion, encryption at rest and in transit; tenant isolation exercised |
| Intrusion response | Detection independent of AI; no destructive or firewall actions without separately signed local policy, TTL, rollback and operator safeguards |
| Supply chain | Signed builds, SBOM, dependency review, independent vulnerability testing, secure update and rollback |
| Operational service | Measured external availability, incident response, verified backup/restore, key revocation and rate limits |
| AI governance | Read-only investigation bounded by Jani broker; no autonomous privileges from hostile logs, wake events or model replies |

## Three verification levels

- **Reference laboratory (P0):** test private protocol reference on real TLS socket and loopback central service, synthetic configured logs and host metrics. This does **not** prove Rust integration or arbitrary real-host coverage.
- **Native pilot (P1):** compiled Rust client and Rust central gateway, signed device provisioning, live Linux systemd/journal/container inventory, rotation-safe cursors, offline/restart/saturation tests, limited MSP tenants and human on-call.
- **Production service (P2+):** HA event durability, tenant privacy boundaries, certificate revocation, penetration testing, multi-host and multi-OS acceptance, production restore drills, defined SLOs and maintained signed releases.

Every gate must provide a commit/build identifier, policy revision, platform, test case, observed behavior, failure transcript, reviewer and artifact hash. A generated synthetic case or an AI statement alone does not pass a gate.

## Anti-disruption rule

High-risk remediation stays disabled at all times until a separate enforcer meets human authorization, management-network exclusion, bounded TTL, break-glass and recovery evidence requirements.

## Required documentation maintenance

Changes to model gateway, tool broker, auth policy, journal, remote integration, operating system collectors or monitoring transport require security/coverage review before any status change from proposed.
