---
id: JANI-CORE-004
title: "Component contracts"
status: implemented-partial
owner: Cargo.toml
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - Cargo.toml
tests: []
---

# Component contracts

Jani is a Rust virtual Cargo workspace with an explicit trust kernel and no mandatory coordination runtime.
## Package boundaries
| Package | Purpose | Must not do |
| --- | --- | --- |
| backend/core | Types, governance, identity, policy, audit, budget, approvals | Arbitrary model execution |
| backend/server | CLI, agent loop, model gateway, trusted broker, sandbox and bridges | Skip broker or policy for new effect paths |
| backend/store | Transactional local SQLite and outbox | Claim remote effect atomicity |
| backend/mcp | Verify signed contracts, delegation and receipts | Offer agent-side human commit |
| backend/native | Bounded metadata, planning, context and evidence tools | Execute unmediated source effects |
| backend/mirror | Evidence-linked retrospective critique | Automatically self-apply suggestions |
| tools/jani-check | Workspace architecture checks | Runtime authorization |
| tools/jani-claims | Markdown/source discrepancy detection | Certify semantic implementation solely from prose |
## Interface evolution
- Every new model function is schema-closed, capability-scoped and mediated by the broker.
- Breaking external records require explicit versioning, compatibility tests and rollout instructions.
- Retained source references and meaningful negative tests are required for guarantees.
## Known gap
- Rust internal modules do not constitute a stable external ABI or plugin SDK.
## Acceptance
- Validate workspace members, boundaries, protocol compatibility and no new unmediated effect routes.

## Change control
Source changes require review against negative tests; model assessments do not replace execution evidence.
