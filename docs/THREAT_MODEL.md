---
id: JANI-SEC-001
title: "Threat model"
status: implemented-partial
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/sandbox.rs
tests: []
---

# Threat model

**Status: implemented-partial.** This contract describes existing boundaries alongside explicitly identified limitations.

**Trust boundary:** repository files, model replies, skills, alerts, third-party tools, plugins and MCP results are untrusted. They never grant rights.
## Threat catalogue
- Prompt injection in source/tool output must not become new system instruction or modify effective permissions.
- Privilege laundering through forged user roles, cross-tenant grants, expired delegations or unapproved source results must be denied by signed policy and receipts.
- Secrets must not be inherited by untrusted child processes; model egress requires classification, redaction and provider/purpose approval.
- Traversal, symlink, hardlink, TOCTOU races, pre-staged Git edits and malicious build scripts threaten workspace integrity.
- Missing audit writes, forged evidence, stale lease tokens and unknown remote outcomes must never appear as successful completion.
## Controls and gaps
- Broker policy, protected paths, source-signed MCP receipts, sandbox fail-closed selection and hash-chain journaling implement a local trust kernel.
- Plaintext local state, non-anchored audit, weak Windows/macOS isolation, no key revocation service and incomplete race protection remain risks.
## Required tests
- Exercise prompt injection, forged signatures, tenant crossover, disk failure, malicious file replacement, exfiltration and sandbox escape in disposable environments.
- A high-risk test passes only with independently verified refusal or containment, not a synthetic oracle alone.

## Change control
Source modifications require documentation review and negative tests. Neither passing compilation nor an AI claim alone proves runtime enforcement.
