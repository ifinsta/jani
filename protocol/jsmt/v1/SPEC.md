# JSMT/1 — Jani Secure Machine Transport wire contract

**Status:** open-source reference protocol, experimental; not production certification. This document describes the **implemented Python reference**, plus separately labelled future requirements. Licensed under the repository MIT license. The protocol's name and message vocabulary are Jani-specific; the cryptography is not proprietary. Once this version is published under MIT, future proprietary versions cannot retroactively revoke those rights.

## 1. Scope, participants, and trust boundaries

- **Probe:** explicitly enrolled client on an authorized machine, collecting only configured sources and reporting coverage gaps. A device has exactly one tenant/device identity, not authority from a user-visible hostname.
- **Gateway:** authenticated ingestion service, independently pins a device certificate fingerprint to that exact tenant/device record and durably stores telemetry before acknowledging it.
- **Intelligence:** consumes normalized, authorized telemetry asynchronously. It cannot grant itself permissions by processing a log message. The Python reference has *no* AI connection or remote execution.
- **Local security:** operator consent, host ACLs, logs and existing service availability are independent of network/model availability.

The prototype is not a remote access tool, EDR enforcer, secure login, auto-update service, or proof of total host observability.

## 2. Transport — required in JSMT/1 reference

- Connect to a **HTTPS origin only**, without embedded username, password, query, fragment, or redirects. The probe validates the DNS hostname against the trusted server certificate.
- TLS **1.3 only**; use the platform TLS implementation, not home-grown encryption. The client verifies its configured CA and sends a client certificate; the gateway requires and validates that certificate before any request.
- The gateway additionally checks `SHA-256(DER peer client certificate)` against an independently provisioned registry entry holding tenant and device IDs. Possession of any trusted CA-issued certificate is not sufficient.
- No unauthenticated plaintext HTTP fallback, insecure hostname override, 3xx redirect following, or downgrading transport is allowed.
- Keys/certificates must never be committed to Git. Tests generate ephemeral keys with OpenSSL. The test gateway listens on loopback by default. Do not expose its development PKI or manually provisioned registry on production networks.
- JSMT/1 does **not** implement certificate issuance, rotation, revocation, OCSP, key attestation, enterprise SSO, or trust-root pinning independent of the locally supplied CA file. These are deployment blockers, not promised functionality.

## 3. Endpoint and payload

`POST /v1/ingest` with `Content-Type: application/json`, `Content-Length` between 1 and 262144 inclusive, under an authenticated TLS connection. The actual UTF-8 body must match the fixed envelope. No arbitrary nested tool instructions are accepted.

```json
{
  "version": "jani-jsmt/1",
  "tenant_id": "tenant-a",
  "device_id": "device-001",
  "sequence": 1,
  "observed_at": "2026-10-25T12:00:00Z",
  "metrics": [{"name": "system.uptime", "unit": "s", "value": 120.5, "attributes": {}}],
  "logs": [{"time_unix_nano": 1792929600000000000, "severity_text": "INFO", "body": "service healthy", "resource": {"service.name": "api"}, "attributes": {"jani.log.source": "file:api.log"}}],
  "coverage": [{"service":"api","source":"file:api.log","status":"covered","reason":""}]
}
```

Envelope fields are **exactly** `version`, `tenant_id`, `device_id`, `sequence`, `observed_at`, `metrics`, `logs`, and `coverage`. Extra fields or missing fields fail. Tenant and device IDs: ASCII letters, digits, dot, underscore, hyphen, 1–96 characters. Sequence: integer 1 through 2^63−1 (excluding bool); it is local to `(tenant,device)` and must monotonically increase from 1 without gaps. Max counts: 256 metrics, 200 logs, 256 coverage records per envelope. Canonical serialized body is capped at 256 KiB. Output quotas can further limit collection.

### Metrics vocabulary

The reference accepts only these names and exact units:

| Name | Unit | Value definition |
| --- | --- | --- |
| `system.uptime` | `s` | Host uptime in seconds |
| `system.cpu.time` | `s` | Accumulated CPU time by `cpu.mode` |
| `system.memory.usage` | `By` | Memory used/available with `system.memory.state` |
| `system.process.count` | `{process}` | Number of observed processes |
| `jani.filesystem.used` | `By` | Used bytes for named mount |
| `jani.service.up` | `1` | 0 or 1 observed service running state |
| `jani.service.log.covered` | `1` | 0 or 1 configured log source accessible |

Metric values must be finite numbers; boolean/NaN/Infinity invalid. Attributes are bounded to eight string-to-string pairs with each value at most 100 characters. This is an **OpenTelemetry-inspired normalized subset**, not an OTLP wire encoder or a full semantic-conventions implementation. Label cardinality budgets and per-tenant quotas are future requirements.

### Logs and coverage

Each log record carries `time_unix_nano` integer, severity `TRACE|DEBUG|INFO|WARN|ERROR|FATAL`, redacted and max-4096-character `body`, `resource.service.name`, and a bounded `attributes` object. Recognized secret strings (authorization bearer, password, token, API key, secret assignments) are redacted; this is not sufficient DLP for production.

Each coverage entry carries `service`, `source`, `status`, `reason`. Status vocabulary: `covered`, `partial`, `missing`, `permission_denied`, `unsupported`. Coverage means the **configured source can be read**, not that it emitted every log or that all host services have been discovered. An unconfigured, unreadable, unsupported, rotated or backlogged source must remain visible as a coverage gap. Discovery's denominator includes authorized known services; a missing discovery adapter cannot be reported as complete fleet coverage.

Journald collection and configured file reading exist in the Linux-first sample. It is not a production Windows Event Log, container discovery, multi-path globbing, directory watcher or service-manager inventory implementation.

## 4. Canonical digest and acknowledgement

Prototype canonical bytes are produced by Python `json.dumps(obj, separators=(',', ':'), sort_keys=True, ensure_ascii=False, allow_nan=False).encode('utf-8')`. `payload_sha256 = lowercase SHA-256(canonical_bytes)`. The reference is explicitly Python-interoperability scoped: cross-language floating-number canonicalization has not been standardized. Before releasing native Rust interop, adopt a frozen RFC 8785-compatible canonicalization or define strict per-field byte encoding plus official golden vectors.

After a successful SQLite `BEGIN IMMEDIATE` commit to both accepted `(tenant,device,sequence,digest,json)` and the device cursor, gateway returns **HTTP 200**:

```json
{"version":"jani-jsmt/1","ack_sequence":1,"payload_sha256":"<64 lowercase hex>","duplicate":false}
```

For identical `(tenant,device,sequence,digest)`, return HTTP 200 with `duplicate:true`. For the same sequence and a different digest, return HTTP 409 `sequence_conflict`. For gaps or out-of-order sequences return HTTP 409 `out_of_order` with expected sequence. Incorrect certificate binding returns 403; invalid envelopes 400; invalid content size 413; wrong content type 415; store unavailable 503.

Probe must compare acknowledgement version, sequence, and digest to its **durably queued** payload and delete only the exact acknowledged `(sequence,digest)`. No ACK before server durable commit, no local queue deletion on transport error, no fabricated success on timeout. Exactly-once effects across external systems are *not* claimed; gateway persistence is idempotent for a particular sequence/digest.

## 5. Outages, buffering, and high cardinality

Probe queues canonical payloads in SQLite WAL with synchronous FULL mode. It enforces a 1000-envelope queue ceiling and stops rather than silently overwriting old data. File offsets are durably staged alongside the queued envelope; if the network is down, accepted data is retained for retry. An oversized log batch is rejected before cursor advance, not silently truncated. Continuous collection under queue saturation is not supported; operational gap metrics and operator intervention are required.

This reference does not offer automated backpressure tuning, disk encryption, log rotation inode tracking, arbitrary large fleet throughput, or cross-process multi-writer guarantees. A restarted gateway that loses committed state may reject probes whose sequence has advanced: no automatic replay reset is permitted. Future reconciliation requires a signed, auditable control protocol.

## 6. Security model and explicit negative cases

Validate malicious text as data, reject unknown protocol version, server certificate mismatch, client certificate omission, unregistered certificate, wrong tenant/device identity, replay with changed payload, sequence gap, incorrect units, nonfinite numbers, unbounded message/labels and unredacted recognized secrets. Logs and telemetry must never become Jani model instructions or an authority grant. The protocol has **no remote command execution** message type.

## 7. Forward compatibility and planned extensions

Current version is `jani-jsmt/1`. Changes that modify field semantics, canonical digest, authentication, ACK, ordering or identity binding require a new major protocol version and new interoperability test vectors. Optional additive features in a future design require explicit capability negotiation and fail-closed unknown-feature handling. Planned separate controls: signed enrollment/revocation, certificate rotation, health heartbeat, protocol negotiation, bounded compression, resumable source cursors and policy distribution. **None are supported in JSMT/1 reference today.**

## 8. Reproduction and release proof

From `protocol/jsmt/v1/reference/python` run `python -m unittest discover -s tests -v` with Python 3.11+ and `openssl` available. It launches real TLS 1.3 client/server sockets, verifies mutual authentication and actually commits records to SQLite. Tests include an external gateway process, configured service logs, duplicate/altered replay, offline resumption, missing source coverage and invalid metric units. A passing reference test does not prove actual Jani Rust runtime compatibility or that all services on a real Windows/macOS/Linux host are covered.

See the source-linked compliance gates in [Sentinel release gates](../../../docs/SENTINEL_RELEASE_GATES.md) and the [release catalog](../../../releases/README.md).

## 9. Native Rust client and gateway (experimental)

The Rust crates `backend/jsmt`, `backend/probe` (`jani-probe`), and
`backend/monitoring` (`jani-gateway`) implement a read-only subset of
JSMT/1 with manually provisioned device certificates. The native code and the
reference implementation use the exact v1 envelope and HTTP acknowledgement
schema. The basic vector at `vectors/basic.json` has canonical SHA-256
`ee12c79dc0c57a15ca58bdb663ff2c7d1c1bd72acef2cee173a609d6de0bf405`.
Float canonicalization beyond the vector remains constrained and requires
explicit cross-language compatibility evidence before claiming general interop.

Compile with `cargo build --locked -p jani-probe -p jani-gateway`. The
separate-process integration test is `synthetic/tests/test_jsmt_rust_e2e.py`;
enable it only after compiling native binaries with
`JANI_RUST_E2E=1 python -m unittest synthetic.tests.test_jsmt_rust_e2e -v`.
The test generates short-lived CA and device certs on loopback. Never reuse
those test credentials for production.

### Current native limitations

- Native sources: explicitly configured regular files with durable byte offsets
  plus opt-in Linux `journal:unit.service` sources with stored cursors.
  Linux systemd service listing is inventory, **not** automatic collection.
  Unconfigured services remain unsupported; first journal tail is partial.
  Live journald service-coverage acceptance is still pending.
- No production Windows Event Log or macOS Unified Logging adapters, no
  container/DB-aware source discovery, no automatic certificate
  enrollment/rotation/revocation, no horizontally scalable ingestion or
  central AI event integration.
- No daemon installer, service manager integration, supervised recovery,
  per-tenant rate quotas, automatic updater, signed binaries or EDR blocking.
- SQLite commit precedes ACK, but without centralized replication it is
  not a disaster-recovery guarantee.
- The client queues a maximum of 1000 envelopes and rejects overflow. No
  silent discard is acceptable; operators must monitor spool saturation.

See `docs/PROBE_GATEWAY_GUIDE.md` for manual provisioning, limitations and
service acceptance. The native implementation is not a production release.

## 10. Native operations extensions (pilot source)

In addition to the fixed JSMT/1 wire envelope, the native gateway can
reload an operator-maintained fingerprint registry per request and enforce
`revoked: true` or `expires_unix` for each tenant/device binding.
This is a local enrollment-operator contract, not a change to JSMT/1 wire
semantics. Bounded per-device rates, normalized SQL projections, coverage
incident outbox, and 1–90-day storage retention are implementation details.
No production certificate issuance, PKI lifecycle, cross-platform collectors,
HA or central model-triggered remediation are available.

## 11. Latest-receipt durability across retention

In the native Rust pilot, a committed `device_receipts` checkpoint keeps the
latest `(tenant,device,sequence,SHA-256)` after raw telemetry expires. A retry
of exactly that last committed sequence returns the same authenticated duplicate
ACK when its payload digest matches; changed digest returns HTTP 409
`sequence_conflict`. An old migrated cursor whose raw data is no longer
available has no trustworthy hash and returns `receipt_unavailable`. This is
not historical arbitrary replay and not backup/disaster recovery.
