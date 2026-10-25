---
id: JANI-OPS-009
title: "Native Jani Probe and JSMT gateway pilot guide"
status: implemented-partial
owner: backend/monitoring
version: 0.1.0
last-reviewed: 2026-10-25
source:
  - backend/jsmt/src/lib.rs
  - backend/probe/src/main.rs
  - backend/monitoring/src/main.rs
  - protocol/jsmt/v1/SPEC.md
tests:
  - synthetic/tests/test_jsmt_rust_e2e.py
---

# Native Jani Probe and Gateway — experimental installation and acceptance

This is an **open-source source implementation**, not a supported production installation. It has not been independently validated on the customer's machines. Publishing source code is not equivalent to a compiled/certified release. The previous JSMT/1 Python reference has its own passing tests. Native Rust end-to-end verification must be run independently.

## Trust model

- Gateway accepts TLS 1.3 with a valid client certificate under a preprovisioned CA, **then** checks the exact SHA-256 DER certificate fingerprint in a read-only tenant/device registry. The certificate alone is not sufficient.
- Client requires an HTTPS origin, validates gateway identity with an explicit CA and authenticates via client certificate. It never falls back to HTTP or runs remote commands.
- Gateway acknowledges only after SQLite transaction commit; the probe removes a local pending item only after matching the authenticated ACK sequence and payload digest.
- A registered device's credentials and local sources must be provisioned by an authorized operator. No user-supplied hostname can grant tenant membership.
- No centrally controlled privileged enforcer, endpoint isolation, rootkit detection, host firewall modification, or arbitrary shell exists in this build.

## Build

From the repository root with the pinned Rust toolchain installed:

\`\`\`sh
cargo build --locked -p jani-jsmt -p jani-probe -p jani-gateway
cargo test --locked -p jani-jsmt
\`\`\`

The release publisher now recognizes \`-Type client\` and \`-Type gateway\`.
**Do not publish binaries until native interop, OS log inventory and security
gates pass.** There are no prebuilt client or gateway releases yet.

## Certificates

For an isolated loopback laboratory, use the ephemeral CA/device certificate
creation in \`synthetic/tests/test_jsmt_rust_e2e.py\`. The deployment team must
provision unique, short-lived device keys using an approved PKI. Protect private
keys with OS-managed ACLs and never commit them or ship development root keys.
Certificate enrollment, revocation and rotation are currently missing and
block deployment to untrusted customers.

Calculate the lowercase SHA-256 of the DER-encoded client certificate and
create the registry JSON:

\`\`\`json
{
  "<64-char-certificate-fingerprint>": {
    "tenant_id": "tenant-a",
    "device_id": "device-one"
  }
}
\`\`\`

The literal placeholder is not a valid fingerprint; replace it before launch.

## Start the gateway

\`\`\`sh
cargo run --locked -p jani-gateway -- \
  --bind 127.0.0.1:8443 \
  --ca ./pki/ca.crt --cert ./pki/server.crt --key ./pki/server.key \
  --registry ./pki/devices.json --db ./state/gateway.sqlite
\`\`\`

Binding to a non-loopback address requires explicit \`--allow-remote\`. This
flag is not a production security certification: first provision trusted
certificates and an isolated network boundary. The gateway is a single-node
SQLite ingest service without redundancy, managed enrollment or quotas.

## Configure monitored services

\`\`\`json
{
  "services": [
    {"service": "api", "path": "/var/log/myapp/api.log"},
    {"service": "worker", "path": "/var/log/myapp/worker.log"}
  ]
}
\`\`\`

On Linux, `{"service":"api","path":"journal:api.service"}` is also allowed.
A bounded read-only `journalctl` call collects only that unit's events with
a durable cursor; the first tail is marked partial because it cannot establish
complete earlier history. Journal permissions and OS behavior need live
verification before any claims of full service coverage.

Sources must be explicitly authorized and readable by a minimally privileged
local identity. The Linux prototype also attempts to enumerate systemd unit
names, reporting unconfigured services as unsupported. Missing files and denied
access produce non-success coverage statuses. This is **not a claim of every
system service log being aggregated**. Discovered services lacking an adapter
remain gaps.

## Start the probe

\`\`\`sh
cargo run --locked -p jani-probe -- \
  --gateway https://localhost:8443 \
  --tenant tenant-a --device device-one \
  --spool ./state/probe.sqlite --sources ./services.json \
  --ca ./pki/ca.crt --cert ./pki/device.crt --key ./pki/device.key
\`\`\`

Add \`--watch\` for repeated collection every 30 seconds. The probe requires
an OS-level supervisor for automatic restart, bounded memory and watchdog
verification. Failed delivery leaves a durable queue; a one-shot run returns
nonzero rather than claiming transmission succeeded.

## Proof and required next gates

\`\`\`sh
cargo build --locked -p jani-probe -p jani-gateway
JANI_RUST_E2E=1 python -m unittest synthetic.tests.test_jsmt_rust_e2e -v
python -m unittest discover -s synthetic/tests -v
\`\`\`

The opted-in test launches **separate Rust processes**, creates real mTLS
credentials, sends configured application log lines and normalized metrics,
checks incomplete source coverage in the database, verifies ACK, cross-language
Python→Rust envelope compatibility, replay conflict and an unregistered
certificate refusal.

Even passing the test does not prove log coverage across an arbitrary real
host, performance, HA, secure enrollment, Linux/macOS/Windows collectors,
auto-updates, supply-chain integrity or intrusion prevention. Run the separate
[release gates](SENTINEL_RELEASE_GATES.md) before marketing those capabilities.

## Native observability and identity operations

Gateway projectors now persist normalized metric samples, service log rows,
latest per-service source coverage, incident state and an immutable incident
outbox in the same SQLite transaction as the raw envelope and ACK sequence.
The `jani-gateway inspect --db ... --tenant ...` command shows aggregate
counts locally. Native source-file rotation/truncation is detected as partial
coverage, not success. Probe queues are bound to their enrolled tenant/device
and capped at 1000 records or 64 MiB of queued payload.

Enrollment/revocation: `tools/sentinel_registry.py` validates a separately
issued CA client certificate, records its SHA-256 DER fingerprint, tenant,
device and expiry, then atomically updates the registry. The gateway reloads
it for every connection request. Revoking is not a replacement for trust-root
rollover, key custody or a managed PKI platform.

Deploy Linux service templates from `deployment/systemd/` only as a
laboratory pilot after passing a native build and TLS interoperability checks.
Gateway samples, raw envelopes, service logs and incident outbox follow a
configurable 1–90-day retention horizon (default 7); live incident state and
anti-replay sequence cursors persist. Databases and backups need encrypted
storage and an independent verified restore plan.

See [Sentinel operations compliance](SENTINEL_OPERATIONS.md). Windows Event
Log, macOS Unified Log, certified journald completeness, container logs, managed key
issuance and HA remain explicit blockers for cross-platform production.

## Lost acknowledgement after telemetry retention

The pilot gateway now commits a device's latest sequence and SHA-256
acknowledgement receipt separately from its raw telemetry. This lets a probe
safely retry its **latest** committed message after retention has removed that
raw row: matching payloads receive the same authenticated ACK; mismatched
digests receive HTTP 409. Old cursors whose raw payload had already been
purged before this migration have no recoverable digest; they fail closed as
`receipt_unavailable`. A full cursor-reconciliation protocol remains future work.
