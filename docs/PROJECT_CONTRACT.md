---
id: JANI-CORE-001
title: Project contract
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/agent.rs
tests:
  - backend/server/tests/e2e.rs
---
# Jani project contract

Jani is a standalone Rust coding agent. Model, skill, source and tool response content are not authority.

## Invariants

- All model-visible effects are mediated by the Rust broker, policy and operator ceilings.
- Enterprise policies and bounded work contracts narrow, never widen, permissions.
- An alert, plan, or simulated result is not proof of completed work.
- Hivewire and any external coordinator are optional, not required for execution.
- Final human-only submissions are not delegated to Jani.

## Current limitations

No distributed coordinator, cross-host checkpoint handoff, automatic model restart, managed identity control plane or fully isolated Windows/macOS execution is implemented.

## Acceptance

Verify default-deny, out-of-scope write denial, credential protection and evidence-linked completion in disposable test runs. Changes to the agent and broker require review.
