---
id: JANI-REL-001
title: "Release acceptance"
status: proposed
owner: .github/workflows/ci.yml
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - .github/workflows/ci.yml
tests: []
---

# Release acceptance

**Status: NOT CERTIFIED.** Synthetic input generation proves corpus integrity, not runtime correctness or production readiness.
## Build and code quality
- Require deterministic formatting, workspace unit/CLI/E2E tests, zero Clippy warnings, architecture checks and documentation/claim conformance.
- Require platform-scoped evidence on supported Linux/Windows runners; failure to support isolation on a platform is a documented denial, not an invisible downgrade.
## Mandatory security gates
- Zero unauthorized read/write, unapproved model egress, agent user-only commit, cross-tenant leak, forged authorization, bogus audit success or silent sandbox fallback.
- Test each material fault hook through a real bounded Jani execution in a disposable environment; simulated expected outcomes are never pass evidence.
## Evidence receipt fields
- Commit SHA, binary digest, test runner identity, platform, scenario ID, expected invariant, observed decision, stdout/stderr digest, artifact checksum, time and human reviewer.
## Operational release gates
- Verified backup/restore, managed key rotation and revocation, incident response runbooks, resource limit soak tests, reproducible build provenance and safe rollout/rollback.
## Release decision
- Any missing critical evidence is a blocker. Reviewers may restrict an experimental release to supervised local read-only use; they may not label it enterprise production-ready.
## Current gaps
- Current CI runs relatively small synthetic smoke generation; it does not execute the entire 5-million-case corpus through Jani.

## Change control
Source changes require review against negative tests; model assessments do not replace execution evidence.
