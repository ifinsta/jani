---
id: JANI-SEC-002
title: "Authorization matrix"
status: implemented-partial
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/policy.rs
tests: []
---

# Authorization matrix

**Status: implemented-partial.** This contract describes existing boundaries alongside explicitly identified limitations.

Authorization is the intersection of operator ceilings, signed identity, signed policy, optional restricting work contract, task scope, capability and tool-specific restrictions.
## Operation matrix
| Action | Required authority | Responsible executor |
| --- | --- | --- |
| Worktree read | policy-approved scope and disclosure classification | Rust broker |
| Workspace write | operator opt-in, relative path scope, policy, required approval | Rust broker |
| Command execution | operator exec opt-in, command allowlist, risk profile and sandbox | Rust broker |
| Native metadata mutation | native operation policy, task/tenant identity | native bridge via broker or CLI |
| Planner verification | signed human identity and explicit approval | human CLI only |
| MCP read/prepare | initiating user, signed delegation and server contract, signed result | governed MCP CLI |
| MCP user_commit | independently authenticated initiating human | source application only |
## Invariants
- Unknown actions or entitlements are denied; a wake event or text-labelled approval creates no authority.
- Signed grants cannot bypass a user-only commit restriction. Source servers must independently recheck current user rights.
## Known limitations
- CLI approval roles are caller asserted; there is no centralized IdP, immutable key registry or real-time revocation broker.
## Acceptance
- Reject self-approval, stale grants, revoked source permissions, wrong tenant, untrusted receipt and user_commit from agent.

## Change control
Source modifications require documentation review and negative tests. Neither passing compilation nor an AI claim alone proves runtime enforcement.
