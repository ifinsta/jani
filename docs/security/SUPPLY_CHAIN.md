---
id: JANI-SEC-008
title: "Plugin and software supply chain"
status: implemented-partial
owner: backend/core
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/server/src/plugins.rs
tests: []
---

# Plugin and software supply chain

## Software provenance
- Pin Rust dependencies through Cargo.lock and review changes to network, cryptography, sandbox and compiler dependencies.
- Signed plugin manifest discovery verifies integrity metadata; it is not equivalent to a runnable sandboxed plugin execution engine.
## Required controls
- Produce release SBOMs and artifact hashes, verify build provenance, scan licenses and vulnerabilities and sign reproducible distributions.
- Reject unsigned or unsupported plugin manifests and prohibit arbitrary native extension execution by manifest alone.
- Maintain publisher trust and revocation lists, issue advisory notices for compromised packages and isolate third-party workspaces.
## Current gaps
- No complete plugin SDK, WASM runtime, transparency log, publisher revocation or attested build chain is implemented.
## Acceptance
- Tamper manifests, downgrade versions, replace package archives and test that no unsigned plugin becomes invocable.

## Review rule
Track this document against the cited implementation, create runtime receipts for unverified guarantees, and refuse to treat proposals as implementation.
