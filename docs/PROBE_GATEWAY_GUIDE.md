---
id: JANI-OPS-007
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
