---
id: JANI-GOV-006
title: "Documentation index"
status: implemented-partial
owner: project
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - README.md
tests: []
---

# Jani documentation index

The documents below define Jani's implemented boundaries and forward-looking contracts. Read the status and limitations in each file before relying on a feature. Source code and executable tests are the implementation authority, not an AI-authored proposal.

## Start here

- [README](../README.md) — what Jani is, quick start and the authority model
- [COMPONENT CONTRACTS](COMPONENT_CONTRACTS.md)
- [PROJECT CONTRACT](PROJECT_CONTRACT.md)
- [RELEASE ACCEPTANCE](RELEASE_ACCEPTANCE.md)

## Core, security and lifecycle

- [AGENT LIFECYCLE](AGENT_LIFECYCLE.md)
- [AUTHORIZATION MATRIX](AUTHORIZATION_MATRIX.md)
- [CONFIGURATION REFERENCE](CONFIGURATION_REFERENCE.md)
- [DELEGATION PROTOCOL](DELEGATION_PROTOCOL.md)
- [ERRORS AND RETRIES](ERRORS_AND_RETRIES.md)
- [RECOVERY AND REPLAY](RECOVERY_AND_REPLAY.md)
- [STORAGE CONSISTENCY](STORAGE_CONSISTENCY.md)
- [THREAT MODEL](THREAT_MODEL.md)
- [TOOL EXECUTION CONTRACT](TOOL_EXECUTION_CONTRACT.md)

## Sentinel security and monitoring compliance

- [Open-source JSMT/1 communication protocol](../protocol/jsmt/v1/SPEC.md)
- [JSMT/1 reference implementation and integration tests](../protocol/jsmt/v1/README.md)

- [Sentinel client and host monitoring contract](SENTINEL_CONTRACT.md)
- [Sentinel secure transport and service readiness gates](SENTINEL_RELEASE_GATES.md)
- [Sentinel operational compliance and production evidence](SENTINEL_OPERATIONS.md)

- [Native Rust probe/gateway installation and acceptance](PROBE_GATEWAY_GUIDE.md)

## Native tools

- [CHECKPOINT](native/CHECKPOINT.md)
- [CONTEXT](native/CONTEXT.md)
- [EVIDENCE](native/EVIDENCE.md)
- [IMPACT](native/IMPACT.md)
- [INDEX](native/INDEX.md)
- [OPTIMIZER](native/OPTIMIZER.md)
- [PLANNER](native/PLANNER.md)
- [PRUNER](native/PRUNER.md)
- [SCHEDULER](native/SCHEDULER.md)
- [SIMULATOR](native/SIMULATOR.md)
- [VERIFIER](native/VERIFIER.md)
- [WORKSPACE](native/WORKSPACE.md)

## Security operations

- [DATA LIFECYCLE](security/DATA_LIFECYCLE.md)
- [INCIDENT RESPONSE](security/INCIDENT_RESPONSE.md)
- [KEY MANAGEMENT](security/KEY_MANAGEMENT.md)
- [SUPPLY CHAIN](security/SUPPLY_CHAIN.md)
- [TENANT ISOLATION](security/TENANT_ISOLATION.md)

## Integrations

- [HIVEWIRE ADAPTER](integrations/HIVEWIRE_ADAPTER.md)
- [MCP SERVER GUIDE](integrations/MCP_SERVER_GUIDE.md)
- [MODEL PROVIDER CONTRACT](integrations/MODEL_PROVIDER_CONTRACT.md)
- [PLUGIN RUNTIME](integrations/PLUGIN_RUNTIME.md)
- [SKILLS CONTRACT](integrations/SKILLS_CONTRACT.md)

## Operations

- [BACKUP RESTORE](BACKUP_RESTORE.md)
- [CLI REFERENCE](CLI_REFERENCE.md)
- [DEPLOYMENT](DEPLOYMENT.md)
- [INSTALLATION](INSTALLATION.md)
- [RELEASES](../releases/README.md)
- [OBSERVABILITY](OBSERVABILITY.md)
- [PLATFORM SUPPORT](PLATFORM_SUPPORT.md)

## Testing

- [Sentinel two-suite verification: 100 cases](SENTINEL_TWO_SUITE_VERIFICATION.md)
- [EVIDENCE PROVENANCE](../synthetic/EVIDENCE_PROVENANCE.md)
- [EXECUTION ADAPTERS](../synthetic/EXECUTION_ADAPTERS.md)
- [FAULT HOOK COVERAGE](../synthetic/FAULT_HOOK_COVERAGE.md)
- [LOAD AND SOAK](../synthetic/LOAD_AND_SOAK.md)
- [TEST ORACLES](../synthetic/TEST_ORACLES.md)

## Governance

- [CHANGELOG](../CHANGELOG.md)
- [CONTRIBUTING](../CONTRIBUTING.md)
- [ADR README](ADR/README.md)
- [DOCUMENTATION COVERAGE](DOCUMENTATION_COVERAGE.md)
- [ROADMAP](ROADMAP.md)

## Repository policy

- [SECURITY](../SECURITY.md)

## Validation

Use python tools/doccheck.py for inventory/source-reference checks and cargo run -p jani-claims -- check for deterministic Markdown/source assertion checks. Production-readiness needs independent runtime receipts.
