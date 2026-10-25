---
id: JANI-OPS-002
title: "Configuration reference"
status: implemented-partial
owner: backend/core/src/config.rs
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/config.rs
tests: []
---

# Configuration reference

Configuration is resolved in the Rust CLI parser; this document is an operational entry point rather than an independent source of defaults.
## Model/provider settings
- JANI_API_KEY (or OPENAI_API_KEY fallback), JANI_MODEL and JANI_API_BASE configure an OpenAI-compatible Chat Completions provider.
- The provider base must use HTTPS except explicitly allowed loopback HTTP. Never store live API keys in repository fixtures.
## Workspace and effects
- --repo selects a worktree; --state-dir selects persistent state.
- --allow-write requires explicit --write-path relative scopes, with no implicit full-repository editing.
- --allow-exec permits only approved executable commands; --exec-program supports a constrained list of extra test runners.
## Enterprise inputs
- Enterprise runs require a signed --policy, --policy-key trust roots, a signed --identity and a restricting --contract.
- --sandbox-mode and network restrictions must not be used to weaken required confinement; sandbox doctor reports support, not certification.
## Operational rules
- Separate public verification keys from private signing keys and prevent exposure via child environment or diagnostics.
- Review every flag and its precedence directly against the parser when shipping a new release; no remote config control plane exists.
## Acceptance
- Verify --help without credentials, rejection of invalid scopes or programs and refusal of enterprise runs missing required authority.

## Change control
Source changes require review against negative tests; model assessments do not replace execution evidence.
