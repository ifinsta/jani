---
id: JANI-TEST-005
title: "Test evidence provenance"
status: proposed
owner: synthetic
version: 1.0.0
last-reviewed: 2026-11-28
source:
  - synthetic/readiness.py
  - synthetic/attestation.py
  - synthetic/evidence_io.py
  - synthetic/oracle_evidence.py
tests:
  - synthetic/tests/test_readiness.py
---

# Test evidence provenance

## Required evidence envelope
- Case ID and generator digest, Jani source commit and executable hash, host OS/container image, runner identity, test command, injected hook, expected invariant, observed outcome, timestamps and output artifact digests.
- Include independent oracle method, audit journal verification, effective policy hash and reviewer decision.
## Trust and retention
- Evidence produced solely by a model cannot self-assert execution or pass status. Sign receipts with a CI identity outside the tested process.
- Preserve failed and ambiguous trials, store redacted data, make artifacts reproducible and enforce retention/access rules.
## Readiness checks
- Reject receipts with missing mandatory source hashes, mismatched runner identity, invalid signatures, incomplete execution or unsupported platform assumptions.
- Do not treat generated cases, simulated runs and a green CI build as interchangeable evidence.

## Verify a release campaign

```text
python -m synthetic.readiness --corpus fixtures --receipts evidence/receipts.json --source-commit FULL_COMMIT --campaign-id RELEASE_RUN --trust approvals/trust.json --openssl-bin /absolute/path/to/openssl
```

The operator selects the commit and campaign ID, and supplies trust settings and a trusted OpenSSL 3 executable outside the receipt, artifact and corpus directories. Use operator-owned snapshots that remain immutable throughout verification. Artifact paths are relative to the receipt directory unless `--artifact-root` is supplied. Paths use forward slashes and plain files. The verifier checks the bytes it consumes against each signed descriptor.

### Trust settings

`jani.release-trust/v1` contains `schema`, `corpus_manifest_sha256`, `generator_sha256`, `binary_sha256`, `runners`, and `reviewers`. Verify the corpus against the approved generator with `python -m synthetic.generate verify --output fixtures` before pinning the manifest and generator package digests. Pin the expected candidate binary for each platform. The `binary_sha256` map requires `linux` and `windows`. Include `macos` when the campaign contains macOS profile executions. Register each runner and reviewer by a distinct ID with a `public_key` of 32 Ed25519 bytes as lowercase hex and a `gates` array of allowed gate names. Runner entries also specify `platform`. A `matrix` runner is limited to assembling `synthetic_oracle_execution` evidence. The verifier refuses reused public keys across identities or roles.

### Gate envelope

`jani.release-evidence/v2` contains a `gates` object keyed by the mandatory gate names. Each envelope contains `payload_b64`, `runner_signature`, and `reviewer_signature`. Decode `payload_b64` to the original UTF-8 JSON bytes. Both parties sign the ASCII bytes `jani.release.gate/v1` followed by one NUL byte and those original JSON bytes. Signatures are 64 Ed25519 bytes as lowercase hex. [OpenSSL Ed25519 signing and verification](https://docs.openssl.org/3.0/man1/openssl-pkeyutl/#ed25519-and-ed448-algorithms)

The `jani.release-gate/v1` payload includes `schema`, `gate`, `source_commit`, `campaign_id`, `corpus_manifest_sha256`, `generator_sha256`, `runner_id`, `reviewer_id`, `platform`, `binary_sha256`, `environment`, `command`, `started_at`, `finished_at`, and `reviewed_at`. Use `runtime`, `passed`, and `approved` for `execution`, `result`, and `review_decision` on an accepted gate. `command` is a nonempty argument array. Times use UTC `YYYY-MM-DDTHH:MM:SSZ` and must be ordered. `artifact` contains `path`, `sha256`, and `bytes`.

`binary_sha256` contains exactly the expected digest for the runner's platform. The Linux and Windows security gates require their respective platforms. The matrix oracle gate binds the full expected binary map. Other gate artifacts can contain the logs and receipts appropriate to the gate. Their runner and reviewer attest to the meaning of those contents, and the verifier checks their byte integrity.

### Exact oracle coverage

The oracle gate artifact is a gzip JSONL ledger with one observation per corpus case in manifest order. Each row contains `id`, `input_sha256`, `execution`, `status`, `adapter`, `decision`, `fault_hook`, `invariant`, `runner_id`, `platform`, `binary_sha256`, `stdout_sha256`, `stderr_sha256`, `audit_sha256`, and `effective_policy_sha256`. The input digest covers the exact decompressed fixture row including its newline. A qualifying row reports `execution: "runtime"` and `status: "PASSED"` with true `invariant_satisfied`, `audit_verified`, `no_unapproved_side_effect`, and `no_secret_leak` observations. Named Linux, Windows, and macOS profiles require their native platforms. The verifier compares the decision, hook, and invariant with the approved fixture. Reviewers check the underlying bytes referenced by the four per-case output, audit, and policy digests before signing the ledger.

The matrix payload includes `oracle_runs`, keyed by each contributing native runner ID. Each entry contains `payload_b64` and `signature`. The native runner signs the ASCII bytes `jani.release.oracle-runner/v1` followed by NUL and the original UTF-8 JSON bytes. The `jani.release.oracle-runner/v1` payload has `schema`, `runner_id`, `platform`, `campaign_id`, `source_commit`, `corpus_manifest_sha256`, `generator_sha256`, `binary_sha256`, `observed_cases`, `observations_sha256`, `environment`, `command`, `oracle_method`, `started_at`, and `finished_at`. The native start and finish must be inside the matrix run's window. Each row inherits its runner's signed environment, command, and oracle method. The runner's observation artifacts retain the actual per-case trace and timing. Here `binary_sha256` is the runner's single binary digest. `observations_sha256` hashes the concatenated decompressed ledger rows attributed to that runner in ledger order, with each row's newline preserved. The matrix runner and reviewer sign their envelope including these native statements.

Unique coverage is credited only after all fixture shards, observations, native batch signatures and digests, and the compressed ledger verify through EOF. The release minimum remains five million cases. A skipped case or a failed final row prevents the oracle gate from contributing any qualifying execution count.

## Review
Reconcile source-mapped hooks with actual executable adapters and signed independent test evidence. Runner and reviewer key custody, runtime observation, fixture approval, and interpretation of operational gates remain the responsibility of the selected authorities. The four-hook CLI runner produces preflight diagnostics. The local runtime runner writes a separate, unsigned format with retained traces and explicit skipped cases. [Runtime adapter contract](EXECUTION_ADAPTERS.md)
