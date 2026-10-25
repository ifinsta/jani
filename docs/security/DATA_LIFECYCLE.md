---
id: JANI-SEC-006
title: "Data classification and lifecycle"
status: implemented-partial
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/data.rs
tests: []
---

# Data classification and lifecycle

## Ingress and classification
- All read content receives a provenance and classification before entering provider-bound context. Detect sensitive strings and redact known secrets.
- Model egress is governed by allowed provider, region label, purpose and classification. Empty allowlists deny; a literal star is an explicit wildcard.
## Lifecycle requirements
- Record a lineage link from source to derived context, decisions and produced artifacts; generated AI text inherits restrictions from its sources.
- Define retention classes for logs, checkpoints, evidence, prompts, approvals and temporary tool output.
- Support user-requested deletion where lawful, encrypted backups, legal holds and independent erasure evidence.
## Present limitations
- The existing lineage field is not populated end to end; no enforced retention/deletion service, independent region attestation or full DLP exists.
## Acceptance
- Test classified output through nested JSON escaping, model input redaction, unauthorized provider egress and data deletion under active legal hold.

## Review rule
Track this document against the cited implementation, create runtime receipts for unverified guarantees, and refuse to treat proposals as implementation.
