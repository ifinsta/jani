---
id: JANI-GOV-005
title: "Documentation coverage and claims"
status: implemented-partial
owner: project
version: 1.0.0
last-reviewed: 2026-10-25
source:
  - tools/jani-claims/src/main.rs
tests: []
---

# Documentation coverage and claims

## Documentation system
- The authoritative source of required new contracts is docs/documentation_manifest.json. Each listed document has frontmatter ID, title, status, owner, version, last-reviewed date and source/test path lists.
- Legacy implementation guides and proposal packs remain in place. A proposal does not become a verified implementation simply because it has a heading or source pointer.
## Status vocabulary
- implemented: code exists and repeatable tests are linked; implemented-partial: some behavior exists and explicit limitations remain; proposed: design only; deprecated: no longer recommended; superseded: replaced by a newer decision.
## Automated checks
- python tools/doccheck.py checks manifest paths, uniqueness, frontmatter, source/test file existence and critical-document completeness.
- cargo run -p jani-claims -- check checks committed deterministic source-backed claims and relative links; AI candidates require human verification.
## Change workflow
1. Change implementation and its tests.
2. Review documents listing modified modules, adjust status and limitations, and add new contract to manifest if necessary.
3. Run documentation and claims scanners; resolve or explicitly triage mismatches.
4. Require independent execution receipts for security behavior, not merely the existence of matching source symbols.
## Known limitation
- Static path existence and source_contains checks are not semantic proof, and current CI cannot guarantee humans reviewed every change.

## Maintenance
Keep this document aligned with implemented code and record uncertainties instead of inventing support.
