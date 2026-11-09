---
id: JANI-GOV-003
title: "Engineering roadmap"
status: proposed
owner: project
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/main.rs
tests: []
---

# Engineering roadmap

## Phase 0 — Trusted supervised local tool
- Stabilize current CLI, broker, identity, signed policy, data classification, local audit and refusal semantics.
- Correct mismatched documentation claims and establish traceable source/contracts/test coverage.
## Phase 1 — Recovery and portability
- Introduce durable tool intent/outcome checkpointing, crash reconciliation, per-task fencing and independent verifier.
- Harden Linux isolation; explicitly refuse unsupported enterprise profiles on Windows and macOS until real confinement exists.
## Phase 2 — Managed integration
- Implement centrally governed trust roots, issuer revocation, token broker, tenant encryption and retention controls.
- Add hardened plugin execution SDK and fuller MCP interop while retaining source-side human commit rules.
## Phase 3 — Operated service
- Secure backups, multi-node state/leases, observability, incident containment, support targets and progressive deployment.
## Self-improvement lab (partial implementation)
- Signed, matched, three-repeat advisory trial comparisons and Mirror-grounded proposals are available through \`jani lab\`.
- Still required: isolated independent test execution, verifier key lifecycle, durable evidence and experiment registry, source patch generation, external review clearance and controlled promotion/rollback.
- Signed self-reported test counts alone never establish production readiness; a separate trusted executor must run the test suite and attest results.

## Release gate
- No phase becomes implemented solely from a completed plan, proposal, synthetic corpus or AI review. Require independently verified behavior and negative tests.

## Maintenance
Keep this document aligned with implemented code and record uncertainties instead of inventing support.
