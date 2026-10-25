---
id: JANI-GOV-002
title: "Changelog"
status: implemented-partial
owner: project
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - README.md
tests: []
---

# Changelog

This page records what changed and what it means for compatibility. It is a development record, not a release certification. Evidence for the current state is in [VALIDATION.md](VALIDATION.md).

## Release status

- **No Git tags and no published binaries exist.** The workspace version in `Cargo.toml` is `0.1.0`.
- This snapshot describes commit `8bb3080` (2026-10-25). Later commits are listed under Unreleased.
- Two commits have `v0.1.1` and `v0.1.2` in their subject lines. They are development markers from earlier work, not tagged or published releases. Do not cite them as versions.

## Unreleased

- Rewrote the root guides so each has one purpose: evaluation and quick start (README), evidence and gaps (VALIDATION), reporting and guarantees (SECURITY), the contribution gate (CONTRIBUTING) and this record.
- Documented `--settings`, `--model-profile` and `JANI_SETTINGS` in the CLI and configuration references.
- Corrected the contribution instructions to use `jani-claims sync` then `check`; `scan` only prints findings.

## Model profiles and documentation contracts — `8bb3080`, `916b90f`

- Added `.jani/settings.json` with named model profiles, a default profile and a `maxSteps` limit. Precedence is CLI flag > environment > settings > built-in default.
- Added `--settings FILE`, `--model-profile NAME` and the `JANI_SETTINGS` environment variable. Relative paths resolve against the worktree.
- Settings are strict: version 1, camelCase, unknown fields rejected, 64 KiB cap, at most 32 profiles, `maxSteps` 1..80. An inline `apiKey` is rejected; `apiKeyEnv` names the only credential source used for that profile.
- Settings cannot grant write or command authority. `doctor` reports the loaded settings file and profile.
- Added a source-linked documentation contract library, `docs/documentation_manifest.json`, and `python tools/doccheck.py` as a CI gate.

**Compatibility:** the settings file is optional, so existing environment-only setups are unchanged. A profile that names `apiKeyEnv` will not fall back to `JANI_API_KEY` or `OPENAI_API_KEY`; this is intentional, to avoid sending one provider's key to another endpoint.

## Twelve native tools — `ae5fcde`

- Added Index, Context, Planner, Pruner, Checkpoint, Verifier, Impact, Scheduler, Workspace, Evidence, Optimizer and Simulator under `backend/native`.
- Exposed them through `jani native TOOL OPERATION` and the governed `native_tool` broker function. They receive no additional permissions, and test execution still routes through the governed `workspace_command`.

## Fault corpus and release gates — `aa13ef5`, `8582661`

- Added a deterministic 5,000,000-case fault-injection corpus covering 23 subsystem domains, 371 failure hooks, 32 mutation variants and 17 profiles, with sharded output, SHA-256 manifests and a coverage summary.
- Added a readiness evaluator in `synthetic/readiness.py` and tightened release-acceptance evidence requirements.
- CI generates and verifies small samples only. Generating a corpus is **not** execution evidence for Jani.

## Documentation claims audit — `90194a2`

- Added `tools/jani-claims` to check Markdown links, source references and explicit source-backed assertions, and to write discrepancies to `claims/`.
- Added `scan`, `sync`, `check`, `strict` and `queue`. AI-submitted review candidates are never treated as verified facts.

**Compatibility:** changing Markdown can make the claims register stale. Run `cargo run -p jani-claims -- sync` before `-- check`.

## MCP governance — `79b3c5a`, `57f43db`, `fa9ef14`

- Added signed, fail-closed MCP delegation: verified initiator identity and task roles must match an approved server contract, and results require signed authorization and classification receipts.
- Agent invocations of user-only commit tools are always blocked.
- Rejected wildcard grants and unsafe signed endpoints.
- Only the explicitly authorized `tools/call` transport is supported.

## Mirror retrospective critique — `af74c7d`, `f475136`

- Added deterministic review of recorded runs, run comparison and evidence-linked improvement proposals that are saved only on request.
- Added tenant boundaries and journal-tamper tests. Mirror does not execute or self-apply recommendations and has no model-client dependency.

## Workspace, store and attention lifecycle — `c79729b`, `e36f9c9`, `ff4b75a`

- Introduced the virtual Cargo workspace and a transactional attention store.
- Added local `agent`, `alerts` and `wake` commands, delivery leases and replay-safe publish keys. Alerts never authorize or launch tools.
- Aligned package layout with the `backend/*` workspace convention.

## Governed agent vertical slice — `c5017f9`

- Added the broker-mediated agent loop, signed tenant-bound policy and identity, work-contract narrowing, audit journal, persisted budgets, single-use approvals, data classification and fail-closed egress.
- Added the enterprise implementation review documenting what is enforced and what is missing.

## Initial standalone agent — `8f2e70d`, `9cdc3a1`, `cefa29d`

- Added the first standalone skill-aware Rust coding agent with a bounded model/tool loop and scoped writes.
- Added architecture, testing and validation documentation plus the MIT license.
- `809c695` (`v0.1.2`) broadened hardening and tests across configuration, crypto, MCP, Mirror, native tools and attention, and added `Cargo.lock`; `e3d1f18` removed an archived synthetic data file. `70b2790` (`v0.1.1`) was a marker commit with no functional change.

## Compatibility notes

- Rust 1.95.0 is pinned. The root workspace has non-default members, so use `--workspace` for full tests and lint.
- Python 3.11 is required only for synthetic tooling.
- Settings affect model selection and limits only. They never replace `--allow-write`, `--write-path`, `--allow-exec` or signed policy.
- Documentation with frontmatter must be registered in the manifest; claims must be synced after Markdown changes.
- No migration path exists from an earlier published release because none has been published.

## Known exclusions

- No executable plugin runtime.
- No portable or certified OS sandbox; Windows and macOS cannot satisfy enterprise isolation profiles.
- No managed identity control plane, key rotation/revocation, encrypted tenant storage or signed execution receipts.
- No durable model checkpoint/resume.
- No certified execution of the five-million-case fault corpus.

## Release discipline

When a release is actually tagged, record at minimum: the commit, binary digests, platform and runner, migration or compatibility implications, security advisories and links to execution evidence. Do not summarize a release with test counts alone. A breaking contract change needs a version note and either a migration path or an explicit refusal path. No release may be labeled enterprise production-ready without the evidence required by [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md).

## Maintenance

Keep this document aligned with implemented code and record uncertainties instead of inventing support.
