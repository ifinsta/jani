---
id: JANI-TEST-002
title: "Fault-injection execution adapters"
status: implemented-partial
owner: synthetic
version: 1.0.0
last-reviewed: 2026-11-28
source:
  - synthetic/runner.py
  - synthetic/runtime_runner.py
  - synthetic/runtime_harness.py
  - synthetic/runtime_adapters.py
tests:
  - synthetic/tests/test_runtime_runner.py
---

# Fault-injection execution adapters

## Adapter contract
- Inputs: synthetic case ID, reproducible seed, hook, mutation, operating profile, expected invariant and isolated test environment.
- Action: materialize permitted fixture, inject a measurable failure, launch actual Jani executable with bounded scope and capture exact observed effects.
- Outputs: execution level, program exit, stdout/stderr hashes, audit verification, platform, artifact hash and whether an independently measurable oracle passed.
## Existing implementation boundary
- `synthetic.runner` is the four-hook CLI preflight runner. `synthetic.runtime_runner` runs selected corpus rows against a real Jani executable. It checks every row in a selected shard against the approved generator and verifies the compressed shard digest before executing any row from that shard.
- Audit adapters check a corrupted digest, a deleted interior event, an inserted event, reordered events, and incomplete JSONL. They create a verifiable journal and check refusal from both `audit verify` and the next agent startup. They compare the journal and anchor before and after each command and record loopback connections, including connections with no valid model request. Deleting complete tail events from a closed chain requires a separately retained cutoff.
- Tool adapters check a missing write grant, an unknown tool, and crossing a tool-call limit. The corresponding controls check an exactly scoped write or a permitted read. They link the provider tool result to the native invocation, resource digest, decision and reason code, then verify the observed file or budget. A successful process exit alone cannot pass the adapter.
- The local adapters exercise `cold_start` baseline and consistent clean controls with one tenant and one fault. The fixture binds the tenant, principal, task and run. The remaining context fields do not simulate clock offsets, concurrency, classification or payload extremes. Compound faults, other profiles including `offline_recovery`, unsupported mutations and clean rows whose generated decision conflicts with the control are reported as skipped.

## Run a local range

Use an approved local binary and manifest SHA-256. Provide a fresh output directory outside the corpus, with an existing parent. The example range contains audit, write-grant and budget fixtures in the default corpus.

```text
python -m synthetic.runtime_runner --binary PATH_TO_JANI --binary-sha256 BINARY_DIGEST --corpus synthetic/generated/full_5m --manifest-sha256 MANIFEST_DIGEST --output synthetic/generated/runtime-local --first 11790 --last 23640 --max-executions 24
```

`summary.json` seals the compressed observations after successful report publication. Each attempted row records its exact input digest, expected outcome, observed result and retained trace. The trace contains command arguments and exit status, stdout and stderr bytes, journal and anchor bytes, and the loopback exchange. Treat an output directory without `summary.json` as incomplete. An all-skipped range or a failed adapter returns exit code 1. Invalid input or incomplete report construction returns 2.

The default corpus has no eligible `audit.incomplete_jsonl` row for this profile. Its adapter is tested as a separate native fixture. Local observations use `execution: "local-runtime"` and remain separate from the approved release ledger and signatures. [Release evidence](EVIDENCE_PROVENANCE.md)
## Safety
- These adapters use a disposable workspace, a sanitized process environment and an explicit loopback model. They launch an operator-selected binary and give the model only fixed fixture operations. The timeout applies to the direct CLI process and the one-MiB limit applies to retained bytes per stream. Temporary capture files can grow past the limit between checks. OS-sandbox, real external effects and hostile executable coverage require separate environments and evidence.
## Development requirement
- Expand hooks incrementally by subsystem with narrow adapters and independent replay tests; gate releases on critical coverage rather than bulk count.

## Review
Reconcile source-mapped hooks with actual executable adapters and signed independent test evidence.
