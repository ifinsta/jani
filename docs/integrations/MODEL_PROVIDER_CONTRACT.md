---
id: JANI-INT-005
title: "Model provider and egress contract"
status: implemented-partial
owner: backend/server
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/model.rs
tests: []
---

# Model provider and egress contract

## Wire interface
- Jani uses an OpenAI-compatible Chat Completions interface with JSON function calls and a bounded turn loop. Compatibility varies among model providers.
## Governance
- The model gateway is the only holder of provider API credentials, subject to configured endpoint validation and context data classification.
- Provider, region label, purpose and classification allowlists are checked before egress. Region labels are not independent proof of data residency.
- Returned function names and JSON must be validated by the broker; model text does not authorize effects.
## Reliability
- Current HTTP requests use bounded timeout and selected transient retries; usage tokens are counted where provided, but actual provider billing remains authoritative.
- Do not retry a model response by replaying already-performed external effects. Maintain correlation and tool result records.
## Gaps and acceptance
- No provider SLA guarantee or attested region routing. Test malformed function calls, rate limits, missing usage, injected tool output, credential exfiltration and multi-turn budget exhaustion.

## Review and compatibility
Changes to protocol, manifests, identity or model tools require compatibility and fail-closed negative tests. Do not infer production readiness from a proposal.
