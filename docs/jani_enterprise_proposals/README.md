# Jani Enterprise-Grade Runtime — Proposal Pack

**Prepared:** 2026-10-25  
**Repository reviewed:** https://github.com/ifinsta/jani  
**Repository baseline:** `main`, commit `32feebb2fa79dba23e55a6901c23d45d00f0090b`  
**Status:** Architecture and implementation proposals. A trusted local vertical
slice is now implemented and independently reviewed; the full pack is **not**
implemented. See [IMPLEMENTATION_REVIEW.md](IMPLEMENTATION_REVIEW.md) for the
requirement-by-requirement status, evidence and remaining gaps.

## Read in this order

1. [01 — First-Class Sandbox, revised proposal](01_FIRST_CLASS_SANDBOX_PROPOSAL.md) — mandatory isolation, platform backends, process lifecycle, trusted patching, attestation, migration, threat catalogue.
2. [02 — Built-In Data Governance](02_BUILTIN_DATA_GOVERNANCE_PROPOSAL.md) — classification, model/provider egress, tenancy, DLP, lineage, encrypted storage, retention/erasure and policy.
3. [03 — Organizational Policy & Governance](03_ORGANIZATIONAL_POLICY_GOVERNANCE_PROPOSAL.md) — identities, roles, ABAC, signed rules, approvals, budget authority, audit, incident lifecycle, managed deployment.
4. [04 — Plugin Architecture](04_PLUGIN_ARCHITECTURE_PROPOSAL.md) — versioned capability registry, WASM/sidecar/MCP, provider adapters, manifest, signing, tool broker, SDK, supply-chain security.
5. [05 — Integrated End-to-End Implementation](05_INTEGRATED_IMPLEMENTATION_PLAN.md) — dependency architecture, Rust package layout, stable schemas, 28 PR work packages, gates G0–G5, acceptance demos.

## Architectural decisions

- **First-class trusted core:** Rust policy PEP, data governance/egress gates, sandbox manager, identity and verified capability grants, credential broker, task journal, audit/evidence.
- **Pluggable capabilities:** tool integrations, model providers, WASM transforms, isolated sidecars, mediated MCP servers, storage drivers, optional classifiers and Hivewire adapter.
- **No bypass:** plugins and SKILL.md guidance cannot grant permission, override sensitive data policies, replace sandbox enforcement claims or silently weaken enterprise security.
- **Graceful adoption:** keep the existing `jani` CLI and local coding workflows where compatible; managed mode never runs high-risk work unconfined.
- **Evidence-oriented delivery:** use repeatable CI, mock-provider integration, sandbox escape tests, crash recovery, data isolation and approval conformance before marking features production ready.

## Current-state caveat

At the time these proposals were written, Jani was a compact standalone Rust
coding agent with basic permission checks, and its original sandbox proposal was
not implemented. The source has since gained a reviewed vertical slice: signed
policy and identity, data-governance egress control, a mediating tool broker,
hash-chained audit, persisted budgets, exact-binding approvals and
bubblewrap/Job-Object sandbox selection. Plugins are verified but cannot execute;
Windows cannot satisfy enterprise filesystem/network confinement; there is no
IdP, policy administration, signed execution evidence or tenant-grade storage.
The proposals preserve the distinction between **implemented**, **documented**
and **planned** behavior.

## Sources used

- Existing Jani `README.md`, `docs/ARCHITECTURE.md`, `docs/SANDBOX_PROPOSAL.md`, `VALIDATION.md`, `src/agent.rs`, `src/config.rs`, `src/exec.rs`, `src/model.rs`, `src/process.rs`, `src/workspace.rs`.
- Hivewire `README.md`: https://github.com/ifinsta/hivewire
- bubblewrap, Microsoft AppContainer, NIST AI RMF, Open Policy Agent, OWASP agent security, OpenTelemetry, MCP 2026-07-28 tool specification, each linked inside relevant proposal.

## Design acceptance

The proposals should be reviewed and approved as a coherent set. Do not implement sandbox as an optional security plugin, trust plugin-provided classifications to downgrade protected data, or treat Hivewire's local identity labels as enterprise security principals. Conformance tests must be planned alongside each implementation stage.
