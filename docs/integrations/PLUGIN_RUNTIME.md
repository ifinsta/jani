---
id: JANI-INT-004
title: "Plugin lifecycle and future runtime"
status: proposed
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/plugins.rs
tests: []
---

# Plugin lifecycle and future runtime

## Implemented current boundary
- Manifest discovery validates signature/integrity metadata and lists declared capabilities. A plugin is not invocable in this build.
## Proposed runtime
1. Pin plugin publisher, digest and version; reject unknown trust roots and revoked packages.
2. Run untrusted code in a separately confined process/WASM instance with no ambient filesystem, network or provider secret authority.
3. Grant only explicit broker-mediated capabilities and a signed workload context; reevaluate permissions on each call.
4. Enforce per-plugin CPU, wall-time, memory, output, network and data-class limits; isolate tenant state.
5. Journal invocation, denial, external effects and plugin version; independently verify return classification.
## Failure and rollout
- Revoke/disconnect compromised plugins, stage upgrades, allow safe rollback and include crash recovery tests.
## Acceptance
- Plugin with valid manifest but no executor remains inert; forged signature and native executable attempts refuse.

## Review and compatibility
Changes to protocol, manifests, identity or model tools require compatibility and fail-closed negative tests. Do not infer production readiness from a proposal.
