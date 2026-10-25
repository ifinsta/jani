---
id: JANI-OPS-004
title: "Command-line reference"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/config.rs
tests: []
---

# Command-line reference

## Command families
- run/--print: model-driven task execution subject to operator scope and policy.
- doctor, skills, sandbox, plugins, policy, audit, inspect, approvals, budget, identity and contract: environment, safety and operational inspection.
- agent, alerts, wake: local attention-state operations; neither alert nor wake automatically invokes an LLM.
- mirror: deterministic audit critique and recorded recommendations.
- mcp: governed source access using signed contracts and task delegation.
- native TOOL OPERATION: bounded indexing, planning, context, evidence, workspace metadata and simulation.
## Safety-relevant switches
- --repo and --state-dir select worktree and persistence. --allow-write requires --write-path relative directories; --allow-exec enables governed allowlisted commands.
- --mode enterprise, --policy, --policy-key, --identity and --contract supply enterprise authorization.
- --approve in native planner verify is permitted only for a verified human identity.
## Rule
- This file is not a replacement for the parser or --help. When adding CLI arguments, document their exact rejection conditions and negative tests.
## Acceptance
- Test unknown flag, missing option argument, invalid scope, protected credential path and unavailable sandbox.

## Maintenance
Review exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
