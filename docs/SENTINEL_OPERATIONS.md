---
id: JANI-OPS-008
title: "Sentinel service operations, compliance and release evidence"
status: implemented-partial
owner: backend/monitoring
version: 0.2.0
last-reviewed: 2026-10-25
source:
  - backend/probe/src/main.rs
  - backend/monitoring/src/main.rs
  - backend/monitoring/src/store.rs
  - tools/sentinel_registry.py
  - deployment/systemd/jani-probe.service
  - deployment/systemd/jani-gateway.service
tests:
  - synthetic/tests/test_jsmt_rust_e2e.py
  - synthetic/tests/test_sentinel_registry.py
---

# Sentinel operational and compliance acceptance — production gate

**Status: source implementation and laboratory-tested operator registry, NOT
production ready.** These gates remain mandatory even if a release binary
exists. Every monitored host requires informed administrator authorization,
documented data minimization, key stewardship, tenant isolation and an
independent recovery path. No covert user activity monitoring.

## Implemented source capabilities and limits

| Capability | Status | Evidence target |
| --- | --- | --- |
| TLS 1.3 device transport and exact certificate pin | Source implemented | Independent Rust build + actual mTLS negative tests |
| Credential enrolment and hot revocation | Operator script + native registry reload staged | Rotate and revoke live on running gateway |
| Normalized samples, logs and coverage | Atomic SQLite projections in native gateway | Query metric_samples, service_logs, coverage_latest |
| Incident capture | Source-status transition outbox | Verify open/resolved with duplicate envelope |
| Data retention | 1–90 day configurable SQLite payload/sample/log/outbox expiration | Test time advance, backup and WAL handling |
| Device rate limit | Configurable 1–10,000 ingestions/minute | Parallel-client load/abuse tests |
| Offline delivery | Per-identity spool, bounded 1000 items/64MiB payload | Reboot, outage and queue saturation exercises |
| Log rotation | File inode/truncation change detected, marked partial | Rotation/drop/loss scenarios on live hosts |
| Linux journald adapter | Explicit units, bounded read, persisted cursor; initial history partial | Live journal permission, replay and rotation tests |
| Linux deployment | Hardened systemd unit templates | Different distros and restart/boot rollback |
| Containers, Windows and macOS logs | **Not implemented** | Mandatory host adapter integration testing |
| AI incident automation, trusted interventions | **Not implemented** | Signed authorization and human final action |
| HA and zero/near-zero interruption | **Not achieved** | External backup, multi-node failover, load soak |

## Data coverage contract

Coverage denominator is the discovered inventory of all services from the
available adapters. For each, require a known source, permission/access test,
tail watermark, offset or cursor, last successful capture, and gap reason.
A detected systemd unit without configured file/journal collection is `unsupported`,
never `covered`. File-source accessibility is **not proof** of events from
other service log destinations. Never market 100% log coverage until this
reconciliation is demonstrated across target OS and container environments.

## Operator actions

- Register a real CA-issued, client-auth permitted device certificate using
  `python tools/sentinel_registry.py enroll --registry /etc/jani/devices.json
  --ca /secure/ca.crt --cert device.crt --tenant tenant-a --device host-001`.
  Do not distribute the issuing CA private key.
- Revoke by fingerprint using `python tools/sentinel_registry.py revoke
  --registry ... --fingerprint <64 lowercase hex>`. Registry replacement is
  atomic; the native gateway re-reads it at each request and rejects revoked
  or expired bindings. CA rollover still requires gateway certificate/trust
  root replacement and a controlled restart.
- Diagnose queued client telemetry using `jani-probe status --spool
  /var/lib/jani-probe/spool.sqlite`. Probe data is never to be silently
  deleted to clear congestion.
- Inspect tenant aggregate counts locally using
  `jani-gateway inspect --db /var/lib/jani-gateway/telemetry.sqlite
  --tenant tenant-a`. This is a trusted **local operator** command, not a
  multitenant network admin API.
- Run with systemd service templates from
  `deployment/systemd/` only after compiling/validating each target binary.
  Grant narrowly scoped log-read ACLs; never turn off sandbox protections
  simply to hide missing-coverage reports.

## Data retention and privacy

The default gateway retention is 7 days, configurable 1–90. Cleanups run
at gateway startup and periodically while requests arrive. The SQLite
`device_cursors`, latest coverage and live incident state survive cleanup
to preserve anti-replay behavior and operations. External backups, filesystem
snapshots, swap and WAL files require a separate proven deletion and at-rest
encryption policy; SQLite FULL synchronous protects commits, not secrets.
Test data subject rights, processing agreements, lawful purpose and access
review before onboarding third-party hosts.

## Outage and operational SLO validation

Define SLOs with pilot measurements (do not invent guarantees):
service availability, fresh-source rate, end-to-end ingestion latency,
durable ACK success, spool backlog age/bytes, missing source ratio,
incorrect intrusion alerts, revocation convergence and recovery duration.
Require chaos tests for gateway disk-full, process kill, TLS expiry, CA
compromise, registry corruption, host log rotation, clock shift, tenant
crossing, network partition, resource exhaustion and source permission
changes. Check that existing customer services continue uninterrupted.

## Mandatory unpassed release gates

1. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
   -- -D warnings`, `cargo test --workspace --all-targets`, `cargo build
   --locked --release -p jani-probe -p jani-gateway`.
2. `JANI_RUST_E2E=1 python -m unittest synthetic.tests.test_jsmt_rust_e2e -v`
   on a real Linux host with OpenSSL, and independent Windows/macOS target
   verification when those platforms are supported.
3. Independently executed live journald/file rotation cursor tests, container adapter, managed PKI
   renewal, revocation, tenant isolation, at-rest encryption and privacy
   redaction audit before opening production enrollment.
4. Signed reproducible supply-chain provenance, SBOM, independent
   penetration test, incident response drill, disaster recovery and
   multi-node availability measurements before high-availability claims.

Existing GitHub-hosted CI failures without assigned runners are *not*
passing evidence. Treat operational quality as **unproven** until a real
test runner produces independently inspectable receipts.
