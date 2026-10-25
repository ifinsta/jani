---
id: JANI-INT-003
title: "Skill ingestion and instruction boundary"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/skills.rs
tests: []
---

# Skill ingestion and instruction boundary

## Format and discovery
- Skills are SKILL.md Markdown documents with name and description YAML frontmatter. The catalog discovers configured and repository-local directories, then reads instructions on demand.
## Security model
- A skill can recommend sequence and tests; it cannot grant new tool capabilities, change the sandbox or assert enterprise approval.
- Skill text is third-party input: treat prompt injection, malicious paths, source changes and covert exfiltration instructions as untrusted content.
- A model request inspired by a skill still follows broker schema, operator ceilings, signed policy, budget and output classification.
## Versioning
- Proposed install processes should pin skill source and digest, review trust level and allow administrator disable/revocation.
## Current gaps
- No signed skills marketplace, independently verified skill attestation or executable skill sandbox is provided.
## Acceptance
- Provide a hostile skill requesting credential disclosure, an invalid SKILL.md and a valid advisory skill; verify no privilege elevation.

## Review and compatibility
Changes to protocol, manifests, identity or model tools require compatibility and fail-closed negative tests. Do not infer production readiness from a proposal.
