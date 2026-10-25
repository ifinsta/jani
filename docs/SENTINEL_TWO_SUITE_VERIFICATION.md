---
id: JANI-QA-001
title: "Sentinel two-suite verification and production acceptance catalogue"
status: proposed
owner: backend/monitoring
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/jsmt/src/lib.rs
  - backend/probe/src/main.rs
  - backend/probe/src/journal.rs
  - backend/monitoring/src/main.rs
  - backend/monitoring/src/store.rs
  - protocol/jsmt/v1/SPEC.md
  - docs/SENTINEL_RELEASE_GATES.md
tests:
  - synthetic/tests/test_jsmt_rust_e2e.py
  - synthetic/tests/test_jsmt_vector.py
  - synthetic/tests/test_sentinel_registry.py
---

# Jani Sentinel — Two Independent Verification Suites

**Version:** 1.0 · **Test plan, not a passing test report** · **Target:** native Rust `jani-probe` and `jani-gateway`, open-source JSMT/1 protocol, Linux-first collector, future Windows/macOS adapters. 

## Mission, acceptance contract and scope

This catalogue contains **100 independently identifiable tests in two suites (50 each)**:

1. **Suite A — Client/host reliability, service-log coverage, metric consistency and local security:** tests the machine-side probe and its impact on ordinary business services.
2. **Suite B — Gateway, JSMT/1 confidentiality, identity, delivery correctness and end-to-end recovery:** tests the authenticated network boundary and central observability store.

The principal acceptance claim is intentionally narrower than “collect every log on every machine”: **all services discovered on a tested host must appear in a coverage ledger, and every in-scope supported and authorized source must have its captured logs delivered and reconciled against an independent source oracle.** Any denied, unsupported, missing, stale, rotated or only-partially-read source must visibly prevent a 100% completeness claim. Unknown inventory must also prevent such a claim. A nonempty readable file does not prove comprehensive service logging.

Mandatory invariants: (i) no client/business service interruption caused by the monitor; (ii) no plaintext transport, insecure fallback or TLS downgrade; (iii) tenant and device identity are independently authenticated; (iv) telemetry does not confer remote execution rights; (v) ACK only after durable storage and exact payload-digest match; (vi) bounded queue never silently discards accepted observations; (vii) metric type/unit/resource context is interoperable; (viii) forensic evidence is verifiable and does not leak credentials.

### Verification classes and release decisions

| Class | Meaning | Required before |
| --- | --- | --- |
| **P0** | Native protocol, basic host operation and immediate security invariants | Any controlled Rust pilot |
| **P1** | Representative live OS workloads, abnormal conditions and operational recovery | Any external customer pilot |
| **P2** | Multi-OS, HA, managed identity, privacy certification, long-soak and penetration gates | General production availability / broad marketing claims |

**Statuses:** `NOT_RUN` (default), `PASS`, `FAIL`, `BLOCKED`, `NOT_IMPLEMENTED`, `NOT_APPLICABLE` (requires approved reason). Neither an invented transcript nor a skipped test counts as PASS. P0 must have **0 FAIL, 0 BLOCKED and 0 NOT_IMPLEMENTED** before a Rust pilot; P1 must pass for each advertised target host before customer enrollment; P2 must pass before corresponding production claims. A failure involving plaintext, wrong-tenant access, silent accepted-log loss, unmediated effects or unavailability of rollback is an unconditional stop.

### Baseline fixtures and prerequisites

- Use a disposable **Linux VM** first, an isolated loopback gateway, an explicitly authorized synthetic test tenant/device and disposable test services: `api`, `worker`, `database`. Add a missing source and an intentionally unreadable source, and independently generate journal events on a test-owned systemd unit. **Never use real customer secrets or perform disruptive network experiments on a production host.**
- Rust toolchain from `rust-toolchain.toml`, SQLite inspection utility or Python `sqlite3`, OpenSSL, Python 3.11+, and permission to inspect the test VM. Capture OS/kernel, CPU/memory, Rust version, Git commit and binary digests. The existing integration harness creates ephemeral PKI and three log fixtures; it is not a substitute for real host tests.
- Use one test root containing `certs/`, `source_logs/`, `probe.sqlite`, `gateway.sqlite`, `registry.json`, a redacted `sources.json`, workload generator output and evidence files. Keep CA private signing keys away from the gateway and probe. Test certificates must never be reused outside this isolated fixture.
- Separate tests requiring **new adapters/features** are marked by their explicit expected outcome; record `NOT_IMPLEMENTED`, do not silently skip, weaken expected outcomes or count them as passing.

### Reproducible baseline commands (existing repository)

From the repository root:

```bash
cargo fmt --all -- --check
cargo build --locked -p jani-jsmt -p jani-probe -p jani-gateway
cargo test --locked -p jani-jsmt -p jani-probe -p jani-gateway
python -m unittest synthetic.tests.test_jsmt_vector -v
python -m unittest synthetic.tests.test_sentinel_registry -v
# Linux-only, requires built debug Rust binaries and openssl:
JANI_RUST_E2E=1 python -m unittest synthetic.tests.test_jsmt_rust_e2e -v
cargo clippy --workspace --all-targets -- -D warnings
```

For the standalone Python reference, independently execute:

```bash
cd protocol/jsmt/v1/reference/python
python -m unittest discover -s tests -v
```

**Important:** the Rust E2E suite is opt-in; merely running `python -m unittest discover` without `JANI_RUST_E2E=1` can yield **SKIPPED** rather than proof. Existing CI runs that fail before a runner executes must be marked `BLOCKED`, not `PASS` or `FAIL` for an individual behavior. `jani-probe --watch` samples every 30 seconds in the present pilot; any tighter sampling rate is a proposed capability, not a CLI flag.

## Suite A — Jani client / host / observer verification (50 cases)

**Oracle:** an independent test workload generates uniquely numbered log markers, known process/service states and bounded load; compare generated IDs to both probe spool and gateway `service_logs`. Verify the probe cannot access unapproved files, no services are interrupted, and `coverage_latest` records every known source and gap. For each case collect host-service health, service-source inventory, probe exit/status, spool state, central projection and resource-overhead traces.

### A1. Installation, permissions and service continuity

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| A-01 P0 | Clean Linux test VM; compile native probe using locked dependencies. | Binary starts, prints correct `--version`; no compiler warnings promoted to errors; commit/toolchain recorded. |
| A-02 P0 | Run probe with no source configuration or missing required CLI flags. | Clear nonzero exit; no collection outside explicit authorization; applications unaffected. |
| A-03 P0 | Install test probe under dedicated unprivileged user. | Probe reads only ACL-authorized sources; no root privilege requirement to mask coverage gaps. |
| A-04 P1 | Install the provided systemd unit in disposable VM and reboot. | Supervised process starts under expected identity; dependent business services retain health. |
| A-05 P1 | Kill probe process during ordinary client request traffic. | External service error rate does not increase beyond measured baseline; supervisor restarts probe without data corruption. |
| A-06 P1 | Restart probe service repeatedly (e.g., 30 restarts). | No runaway restart storm or fd leak; no unexpected duplicated/newly missing accepted log markers. |
| A-07 P1 | Verify OS signal handling and shutdown while spool transaction commits. | Queue is recoverable; no partially committed source cursor; service shutdown is bounded. |
| A-08 P1 | Force CPU/memory contention with a competing application. | Probe honors externally imposed resource limits; application's service-level objective remains satisfied. |
| A-09 P1 | Exhaust permitted probe read permissions on one source. | Source becomes `permission_denied`; other healthy sources continue; no privilege escalation attempted. |
| A-10 P2 | Inspect signed release, package permissions, upgrades/downgrades and unattended rollback. | Trusted build provenance; safe migration/rollback; do not promote package until signed installer and supervisor acceptance exist. |

### A2. Host service inventory and log completeness

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| A-11 P0 | Configure files for `api`, `worker`, `database` and emit one unique marker per service. | Exactly three attributed log entries reach gateway; each source `covered`; compare generated marker set to persisted rows. |
| A-12 P0 | Add nonexistent test service log path. | `missing` status and gap evidence; no falsely positive `jani.service.log.covered` metric. |
| A-13 P0 | Deny read on a configured file using ACL/uid; ensure root would have access. | `permission_denied` status, no client crash and no unauthorized chmod/sudo. |
| A-14 P0 | Configure a symlink to sensitive or rotating outside path. | Refused as unsupported, source not read, no contents leaked. |
| A-15 P0 | Configure duplicate source path under two service IDs. | Configuration rejected; avoid duplicate ownership and double attribution. |
| A-16 P0 | Use malformed service identifier/path relative path/oversized config list. | Fail closed with bounded error; no traversal or out-of-scope read. |
| A-17 P1 | Inventory 10 real test-owned systemd units; configure logs for only six. | Denominator includes all discovered units, four correctly `unsupported`; dashboard never claims 100%. |
| A-18 P1 | Enable explicit `journal:unit.service` source with authorized journald ACL. | Event cursor, severity and timestamp reconcile against `journalctl`; first tail `partial` as documented. |
| A-19 P1 | Emit journal events after durable cursor and restart collector. | No cursor advancement before durable queued record; no missing unique events. |
| A-20 P1 | Force journald tool error/timeout/unreadable journal. | `unsupported` or corresponding gap, bounded subprocess lifetime, no false `covered`. |
| A-21 P1 | Rotate log by inode replacement during collection, then emit new markers. | Rotation recognized as `partial`; no silent loss claim; recover new file and produce loss accounting. |
| A-22 P1 | Copytruncate log between samples. | Truncation reported as `partial`, old/new marker discrepancies measured. |
| A-23 P1 | Append incomplete line without terminating newline then finish later. | Partial line not sent as complete; cursor only advances through complete newlines; final marker delivered once. |
| A-24 P1 | Generate >200 log entries and >128 KiB between samples. | Collector refuses an oversized atomic batch or explicitly reports backlog; no cursor advanced past unqueued records. |
| A-25 P1 | Write long lines, non-UTF8 bytes and mixed Windows CRLF in test-owned file. | Status records partial/truncation or decode substitution as applicable; normalized output bounded; no crash. |
| A-26 P1 | Remove one configured service, rename its unit, then restore. | Inventory and coverage track departure/reappearance; no stale `covered` state. |
| A-27 P2 | Docker/Kubernetes, Windows Event Log, macOS Unified Log and DB-specific logs. | Actual supported adapter enumerates and reconciles all declared in-scope sources per platform; **currently unimplemented adapters remain `NOT_IMPLEMENTED`.** |
| A-28 P2 | Run an independent completeness audit using generated monotonic IDs per source over an hour. | `generated == received_unique + explicitly documented_losses` for every source; all unresolved IDs are release failures. |

### A3. Metric normalization, semantics and freshness

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| A-29 P0 | Collect uptime, CPU time, memory usage, processes and root filesystem. | Each emitted known JSMT metric has exact unit (`s`, `By`, `{process}`, `1`) and sane finite value. |
| A-30 P0 | Force an invalid name/unit, NaN/Infinity or malformed attributes into a protocol fixture. | JSMT strict validator rejects it; no normalized metric row is persisted. |
| A-31 P0 | Verify memory states and CPU modes from `/proc` independent oracle. | Bytes/seconds agree within documented sampling/timebase tolerance; no mislabeled cumulative vs instantaneous values. |
| A-32 P1 | Change system clock and NTP offset in disposable VM. | Observed timestamp provenance remains reportable; stale/future events are not promoted to fresh health evidence. |
| A-33 P1 | Run workloads at 0%, moderate and saturated CPU while collecting. | Measurements correlate with independent OS counters; no impossible negative or nonfinite values. |
| A-34 P1 | Fill filesystem in bounded mounted test volume. | Disk-use status corresponds to independent `statvfs`; probe preserves room for spool and does not fill business partition. |
| A-35 P1 | Stop, start and restart an in-scope service. | Explicit service-up metric is emitted only if implemented with an actual state adapter; otherwise missing support remains visible rather than invented. |
| A-36 P1 | Generate service IDs and metric attributes near label length/count limits. | Bounded cardinality; invalid attributes refused; no unbounded per-source memory use. |
| A-37 P2 | Cross-check same telemetry stream across Linux, Windows and macOS adapters. | Equivalent semantic identity and units across platforms; unsupported platforms are blockers to cross-platform claims. |
| A-38 P2 | Reconcile metric samples after network outage, duplicate retries and clock skew. | Stable resource identity, time ordering and explicitly detected staleness; no double-counting of duplicate envelope. |

### A4. Local queue, privacy and operational resilience

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| A-39 P0 | Stop gateway before probe collects one batch. | A durable pending envelope remains in `probe.sqlite`; one-shot CLI returns failure rather than claiming delivery. |
| A-40 P0 | Restart probe with existing pending batch; restore gateway. | Old sequence sends before new; matching ACK deletes exact local `(sequence,hash)` only. |
| A-41 P0 | Reuse spool with different tenant/device CLI identity. | Local binding rejects reuse; no cross-tenant leakage. |
| A-42 P1 | Fill 1,000-envelope / 64 MiB logical queue or disk quota. | Collector halts before silent overwrite; source offsets not advanced for unqueued material; incident/alert captures saturation. |
| A-43 P1 | Kill process while inserting queue row and source offsets, then restart. | Atomic SQLite state; either old cursor or committed queue+cursor, never an orphan advanced cursor. |
| A-44 P1 | Simulate power loss/ENOSPC, locked SQLite DB and inaccessible spool directory. | Fail closed with actionable error, bounded retries and no disruption to host services. |
| A-45 P1 | Store synthetic API tokens, bearer values and passwords in fixture logs. | Recognized patterns redacted in emitted log body; privacy audit lists patterns NOT covered by current regex. |
| A-46 P1 | Inject hostile “ignore instructions / run shell” strings into service log. | Treated as inert log data throughout probe and gateway; no tool request, command, update or privilege effect. |
| A-47 P1 | Inspect process/env args, fixture cert/key file ACLs, spool and crash logs. | No private keys or secret tokens in telemetry, diagnostics, source control or world-readable artifacts. |
| A-48 P1 | Soak probe with realistic workloads for 24h and 72h (isolated host). | Measured p50/p95/p99 CPU/RAM, log lag, source gaps, queue age; zero unexplained log loss and no service downtime caused by probe. |
| A-49 P2 | Introduce certificate expiry, revocation and fail-closed recovery on running probe. | No insecure reconnect; locally collected evidence retained until authorized recovery or explicit quota limit. |
| A-50 P2 | Execute privacy deletion, spool/backups/WAL handling and customer offboarding drill. | Retention and deletion proven across actual storage layers; documented lawful access, no lingering unprotected secrets. |

## Suite B — JSMT gateway / security / end-to-end verification (50 cases)

**Oracle:** actual compiled Rust processes communicating on TLS 1.3 sockets, authenticated certificate chain + independent registry fingerprint, JSMT/1 strict envelope validation, SQLite `accepted`, `device_cursors`, `device_receipts`, `metric_samples`, `service_logs`, `coverage_latest`, `incidents`, `incident_outbox`. Independently inspect wire TLS negotiation and compare source markers and hashes before/after crash, replay, rotation and storage failures.

### B1. Transport confidentiality and device identity

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| B-01 P0 | Connect valid registered client cert to loopback Rust gateway. | TLS 1.3 negotiated; authenticated `POST /v1/ingest` returns validated ACK; server persistence visible. |
| B-02 P0 | Attempt plaintext HTTP on listener. | No valid ingestion and no plaintext fallback; encrypted transport requirement remains unconditional. |
| B-03 P0 | Force TLS 1.2-only handshake. | Handshake refused; not silently downgraded. |
| B-04 P0 | Connect without client certificate. | Handshake denied before application authorization. |
| B-05 P0 | Connect with valid CA-signed but unregistered client certificate. | Explicit 403 or refused connection; no accepted row. |
| B-06 P0 | Use valid cert for wrong tenant or device in envelope. | 403; unchanged storage and cursor for both identities. |
| B-07 P0 | Use untrusted gateway certificate, wrong hostname or invalid CA. | Probe fails certificate verification; does not send data or delete pending queue. |
| B-08 P0 | Try `http://`, URL credentials, path/query/fragment and HTTP redirects. | Probe refuses invalid origin; no redirect-following or insecure forwarding. |
| B-09 P1 | Revoke a registered certificate via operator registry while gateway is running. | Next request rejected without restart; previously committed data stays intact. |
| B-10 P1 | Expire device registry authorization independently of cert `NotAfter`. | Next ingest denied; unrelated valid device remains operational. |
| B-11 P1 | Corrupt/symlink/swap device registry during read. | Fail closed for enrollment; no default-allow path or stale privilege expansion. |
| B-12 P2 | Exercise production PKI issuance, key compromise, CA rotation, key custody and CRL/OCSP policy. | Revoked/rotated credentials cannot resume under old identity; unattended identity lifecycle formally accepted. |

### B2. Strict wire contract and cross-language semantics

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| B-13 P0 | Recalculate JSMT `vectors/basic.json` canonical SHA-256 in Python. | Matches published `vectors/basic.sha256` and Rust canonical digest; int/float representation pinned. |
| B-14 P0 | Serialize identical envelope with reordered JSON object keys. | Canonical digest identical for supported representations. |
| B-15 P0 | Send unknown `version` or extra field suggesting tool execution. | HTTP 400; no command dispatch, no schema tolerance. |
| B-16 P0 | Remove mandatory fields or send wrong field types, bool sequence or negative sequence. | HTTP 400; no accepted row. |
| B-17 P0 | Send unrecognized metric, wrong unit, NaN/Infinity or nonfinite-derived representation. | Strict rejection; normalized database not polluted. |
| B-18 P0 | Send malformed log resource, invalid severity, unredacted recognized secret. | Strict rejection, including bounds and source attribution. |
| B-19 P0 | Send 0-byte body, 262145-byte body, >256 metrics, >200 logs or >256 coverage entries. | Refusal at parser/validator limits; bounded resident memory. |
| B-20 P1 | Fuzz UTF-8, malformed JSON, surrogate/non-ASCII, numeric exponent and deeply nested payload. | No panic, resource exhaustion or semantic drift; document cross-language floating-number limitations. |
| B-21 P1 | Send duplicate/conflicting HTTP headers, transfer-encoding ambiguity or unsupported media type. | Fail closed; no request smuggling and no ambiguous body parsing. |
| B-22 P2 | Test JSMT/1 against future/older schema, unknown capability and protocol-version negotiation. | Explicit fail-closed compatibility behavior; breaking semantics require version bump and golden vectors. |

### B3. Sequence, acknowledgement and durable delivery

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| B-23 P0 | Fresh device posts valid sequence 1. | HTTP 200 ACK version/sequence/SHA-256 match committed raw canonical payload. |
| B-24 P0 | Replay identical `(tenant,device,sequence,digest)` while retained. | HTTP 200 `duplicate:true`; no duplicate projections or incidents. |
| B-25 P0 | Replay same sequence with changed observed time or body/digest. | HTTP 409 `sequence_conflict`; committed data unchanged. |
| B-26 P0 | Send sequence 3 after sequence 1, then send 2. | First rejected with expected sequence; 2 accepted; no out-of-order acceptance. |
| B-27 P0 | Simulate ACK lost on client after gateway commits. | Probe retransmits identical payload; gateway authenticated duplicate ACK; pending row removed only on match. |
| B-28 P0 | Delete retained raw accepted row **only in fixture** while latest ACK receipt persists; resend. | Matching latest receipt yields 200 duplicate; changed digest 409; never forge a receipt. |
| B-29 P0 | Migrated cursor with raw last item already purged and no receipt. | HTTP 409 `receipt_unavailable`; explicit reconciliation needed, never fabricated ACK. |
| B-30 P1 | Kill gateway between transaction start, commit and socket response using fault hooks. | Never ACK uncommitted effect; restart produces deterministic replay behavior. |
| B-31 P1 | Introduce SQLite lock contention, disk-full, I/O and read-only failures. | HTTP 503 or connection failure; cursor unchanged for noncommitted data; client retains pending. |
| B-32 P1 | Reboot client with multiple pending sequences and restore gateway. | FIFO drain, exactly one persisted canonical row per accepted sequence; no skipped offsets. |
| B-33 P1 | Launch two independent clients sharing device ID/key simultaneously. | Fencing or explicit conflict observed; no undetected corruption or split authority. |
| B-34 P2 | Simulate central restore from stale backup and lost latest sequence. | Signed/fenced reconciliation or declared operational block; no unsafe sequence reset, duplication or silent loss. |

### B4. Central normalized data, coverage and incidents

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| B-35 P0 | Deliver logs from three configured fixture services. | Three correctly attributed rows in `service_logs` with stable tenant/device/source context. |
| B-36 P0 | Deliver normalized host metric subset. | Rows in `metric_samples` match allowed name/unit/value/labels and envelope identity. |
| B-37 P0 | Deliver one missing source and three healthy sources. | `coverage_latest` distinguishes missing vs covered; separate open incident and outbox transition. |
| B-38 P0 | Recover missing source on a later valid batch. | Coverage state updates to covered; incident transitions open→resolved once. |
| B-39 P1 | Replay same incident-triggering payload under same sequence. | No duplicate incident-outbox event or duplicate normalized row. |
| B-40 P1 | Test source status changes: partial→missing→permission_denied→covered. | Correct latest coverage and transition evidence; unsupported inventory remains visible, not counted as covered. |
| B-41 P1 | Multiple devices and tenants with overlapping service names. | Physical/logical storage and query filters prevent cross-tenant reads; no identity aliasing in PKs. |
| B-42 P2 | Independently compare source message IDs, queued IDs, accepted IDs, projected IDs and displayed observations. | Zero unexplained losses/duplications and explicit gap accounting at every hop, at target fleet scale. |

### B5. Adversarial resilience, scale and production compliance

| ID / gate | Stimulus / setup | PASS oracle and mandatory evidence |
| --- | --- | --- |
| B-43 P0 | Attempt non-loopback gateway bind without explicit authorization flag. | Launch refused by default; no unexpected public listener. |
| B-44 P1 | Exceed configured per-device ingestion rate. | HTTP 429 with no accepted sequence advancement; other devices/tenants unaffected. |
| B-45 P1 | Saturate concurrent socket connections and deliver slow/oversized headers. | Bounded threads/memory/timeouts; no crash and no starvation of healthy clients. |
| B-46 P1 | Run a 72-hour many-device traffic and restart soak test. | Report p50/p95/p99 ingest latency, error budget, queue age, retention effects and observed max capacity; no unaccounted business interruption. |
| B-47 P1 | Change retention horizon, purge raw samples and restart gateway. | Purged material absent, latest seq/hash persisted, incident continuity maintained; WAL/backup effects separately disclosed. |
| B-48 P2 | Probe tenant-isolation and privacy boundaries using independent security review. | No cross-tenant read/write through cert, registry, query or retention path; personal-data controls externally verified. |
| B-49 P2 | Test compromise scenarios: stolen device key, hostile logs, malicious updates and enforcer spoofing. | Device can be revoked, logs remain data, no privileged execution message type; signed update/runbook design required before enabling any actions. |
| B-50 P2 | Run disaster recovery, backup restore, multi-node failover and production deployment certification. | Documented RPO/RTO based on real measurements, no split-brain/fake ACK, signed binaries, rollback and independently reviewed incident response. |

## Independent end-to-end acceptance runbook

1. **Freeze build and evidence:** record Git SHA and all binary SHA-256 hashes; verify Rust test vector before starting network traffic. Do not run tests against a user's current production server.
2. **Create isolated PKI and registry:** provision one valid tenant/device, a second CA-trusted but unregistered device, and a second tenant/device. Test operator registry enrolment/revocation; no CA signing private key on either runtime process.
3. **Start native gateway:** bind to loopback; pass explicit `--ca`, `--cert`, `--key`, `--registry`, `--db`; record its stdout/stderr and negotiated connection protocol. CLI can use `--allow-remote` only in an explicitly isolated, authorized remote integration test.
4. **Start native probe:** specify clean `https://localhost:<port>` origin, identity, spool, sources and device certificate. Run once, then in watch mode only on a disposable host. Capture probe return code and queue depth.
5. **Inject known observations:** produce numbered synthetic markers on three files, one missing path, one denied path, opt-in journald event stream, plus independent uptime/CPU/memory measurements. Assign test-owned service identities and timestamps.
6. **Inspect SQLite:** query raw, metrics, logs, coverage, incidents, outbox and cursors; reconcile generated marker IDs against central rows, classifying deliberate gap status as failures to claim completeness, not as successful collection.
7. **Verify transport:** observe TLS 1.3 handshake through a client library/recorded OpenSSL diagnostic and capture no raw application plaintext on a test network packet capture; verify TLS 1.2 and missing-cert attempts are rejected. Packet capture absence of readable payload complements, but does not replace, TLS negotiation proof.
8. **Inject faults:** stop gateway, drop network, simulate ACK loss, revoke cert, corrupt registry, replace/truncate source files, fill spool and restart both processes. Verify sequence/hash receipts, no unauthorized effects, unchanged business workload health and transparent partial coverage.
9. **Repeat on target environments:** independent Linux versions; Windows/macOS only when actual native adapters, packages and deployment support exist. Unsupported OS suites remain unpassed production gates.
10. **Sign off per suite:** independent reviewer inspects machine-generated commands/logs, negative tests and full retention/privacy evidence. Publish build-specific acceptance status; never generalize a lab fixture to all hosts.

### Queryable evidence examples (read-only SQLite)

Run against a **test-only** gateway database after stopping fixture mutation or through a consistent read transaction:

```sql
SELECT tenant, device, sequence, payload_hash FROM accepted ORDER BY tenant, device, sequence;
SELECT tenant, device, last_seq FROM device_cursors ORDER BY tenant, device;
SELECT tenant, device, sequence, payload_hash FROM device_receipts ORDER BY tenant, device;
SELECT tenant, device, service, source, status, reason
  FROM coverage_latest ORDER BY tenant, device, service, source;
SELECT tenant, device, service, COUNT(*) AS messages
  FROM service_logs GROUP BY tenant, device, service;
SELECT tenant, device, name, unit, COUNT(*) AS samples
  FROM metric_samples GROUP BY tenant, device, name, unit;
SELECT tenant, device, service, source, status, last_sequence
  FROM incidents ORDER BY tenant, device, service;
SELECT tenant, device, service, event, sequence
  FROM incident_outbox ORDER BY id;
```

**Coverage equation:** `documented source coverage = supported-and-authorized sources proven captured / all discovered in-scope service log sources`. Also publish the **discovery confidence** (which adapters and environments were inventoried), the unresolved source count and the count of emitted vs received unique markers. Do not compute 100% coverage when the denominator is unknown, even if every configured file reads successfully.

**Security check example** (isolated test gateway; replace paths with disposable test PKI):

```bash
# Positive: record that only TLSv1.3 completes with certificate validation.
openssl s_client -connect 127.0.0.1:8443 -servername localhost \
  -tls1_3 -CAfile ./pki/ca.crt -cert ./pki/device.crt \
  -key ./pki/device.key -verify_return_error

# Negative: must not negotiate obsolete TLS 1.2.
openssl s_client -connect 127.0.0.1:8443 -servername localhost \
  -tls1_2 -CAfile ./pki/ca.crt -cert ./pki/device.crt \
  -key ./pki/device.key
```

OpenSSL exit status alone is insufficient; retain negotiated protocol/certificate verification output and check that application ingest is impossible on the refused channel. Distinguish application-layer 403 from TLS handshake failure.

### Independent evidence record (one per test case)

```yaml
case_id: A-01                  # or B-01 ... B-50
status: NOT_RUN               # PASS/FAIL/BLOCKED/NOT_IMPLEMENTED/NOT_APPLICABLE
release_gate: P0
commit_sha: <actual SHA>
binaries_sha256: {jani_probe: <sha256>, jani_gateway: <sha256>}
os_and_arch: <OS, kernel, CPU arch, runtime>
fixture_id: <sanitized deterministic fixture identifier>
operator_approval: <authorization reference>
command_or_fault: <reproducible steps or runner URL>
expected: <measurable oracle>
observed: <measured facts, not an LLM assertion>
attachments_sha256: [<hash-of-redacted-stdout-or-sql-or-pcap>]
service_impact: <availability, error count, latency deltas>
known_limitations: [<unresolved gaps>]
reviewer: <independent reviewer identifier>
reviewed_at_utc: <real review timestamp>
```

## Release decision and failure response

- **PASS for controlled lab:** P0 all pass and separately executed genuine Rust/HTTPS/mTLS/client-server evidence; no P0 skip counted as success.
- **PASS for external pilot:** P0 + target-specific P1 all pass, explicit host-operator consent, source coverage ledger and incident response staffed. Advertised features may be restricted to supported Linux sources.
- **PASS for production:** applicable P0/P1/P2 all pass with external security assessment, managed PKI, signed release, retention/privacy compliance, HA/failover, proven backup/restore and resource/SLO measurements. Existing roadmap items marked unimplemented are blockers to claims involving them.
- **STOP immediately:** plaintext telemetry, cross-tenant data exposure, unverifiable ACK, silent log/cursor loss, unsafe remote shell/enforcer activity, inability to revoke compromised credentials, or measured probe-caused service outages. Preserve immutable evidence, disable new enrollment, use independent break-glass recovery, and do not auto-remediate with Jani AI.

## Traceability and maintenance

Existing executable coverage includes native unit tests in `backend/jsmt/src/lib.rs`, `backend/probe/src/main.rs`, `backend/monitoring/src/main.rs`, and `backend/monitoring/src/store.rs`; Rust Linux integration in `synthetic/tests/test_jsmt_rust_e2e.py`; golden-vector validation in `synthetic/tests/test_jsmt_vector.py`; registry controls in `synthetic/tests/test_sentinel_registry.py`; and the Python protocol reference in `protocol/jsmt/v1/reference/python/tests/test_end_to_end.py`. **Not every A/B case has an automated implementation.** Add one test function or parameterized case per ID, recording CI jobs and fault-injection evidence, before claiming full acceptance.

Relevant design contracts: [Sentinel client contract](SENTINEL_CONTRACT.md), [Sentinel release gates](SENTINEL_RELEASE_GATES.md), [Sentinel operations](SENTINEL_OPERATIONS.md), [Native probe/gateway](PROBE_GATEWAY_GUIDE.md), and [JSMT/1 wire contract](../protocol/jsmt/v1/SPEC.md). Keep these links and case IDs synchronized with implementation changes. Changing cryptographic semantics or the canonical digest requires a new protocol version and independent vectors—not an undocumented compatibility workaround.
