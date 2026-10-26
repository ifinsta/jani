---
id: JANI-GOV-012
title: "Product and execution excellence programme"
status: proposed
owner: project
version: 1.0.0
last-reviewed: 2026-10-26
source:
  - backend/server/src/tui.rs
  - backend/server/src/agent.rs
  - backend/core/src/policy.rs
tests:
  - backend/server/tests/e2e.rs
---

# Jani — product and execution excellence programme

**Positioning:** Engineering with boundaries. Execution with evidence.

**Purpose:** make the Rust-native, governed coding agent useful every day, while establishing measurable security, reliability and verifiability. This is an implementation programme, **not a completion claim**. Source, independent runtime tests, real-platform evidence and release artefacts determine actual readiness.

## Current baseline (2026-10-26)

- Rust CLI/TUI, scoped broker, policy checks, model egress rules, approval workflow, budgets and local hash-chained journal are implemented in source.
- The terminal supports conversation history within a session, model and command pickers, streamed output and tool entries.
- There is no durable replay of complete model state following process failure, independent task-outcome verifier, executable third-party plugin host, or managed identity/policy control plane.
- On Windows a Job Object is **not** filesystem/network isolation; macOS has no equivalent sandbox. Enterprise isolation-required profiles must continue failing closed.
- A five-million-case generated synthetic corpus is not five million executed agent scenarios. Live-provider and release gates remain distinct.
- Sentinel probe/gateway work is a separate experimental product track, not evidence of general agent readiness.

See [validation](../VALIDATION.md), [architecture](ARCHITECTURE.md), [existing engineering roadmap](ROADMAP.md), [release acceptance](RELEASE_ACCEPTANCE.md) and [Sentinel operations](SENTINEL_OPERATIONS.md).

## Delivery status — 2026-10-26

This register reports committed **source**, not a certified release. A new Windows agent binary was not compiled or published for these changes, so all runtime/CI gates below remain open.

| Track | Source delivered | Still required before phase sign-off |
| --- | --- | --- |
| Phase 0 | Repositioned README, documentation index, normative programme and manifest | Fresh doccheck, claims check, reviewer acceptance |
| Phase 1A | First-run read-only task drafts, terminal authority/profile clarity, Windows probe extensions | Real Windows console and Cargo validation |
| Phase 1B | Bounded multiline editor, Shift+Enter, newline paste, tool status timeline, audit-only completion status | Clippy, accessibility, 40x10/80x24/120x40, real provider and UX E2E |
| Phase 2 | Cooperative cancellation before future tool actions, exact failed-turn context restoration, read-only journal-based recovery inventory | Crash-safe persisted model checkpoint, operation fencing, idempotency, real abort of blocked requests and source-of-truth reconciliation |
| Phase 3 | `jani evidence RUN_ID` journal-verified receipt with explicit unverified-outcome flags | Independent acceptance verifier, signed or externally anchored receipts and acceptance tests |
| Phase 4 | Prior security broker / sandbox refusals retained | OS-level Windows/macOS confinement tests, key rotation, supply chain signatures and external security review |
| Phase 5 | Existing governed MCP and non-executable plugin manifest registry retained | Isolated executable plugin runtime, expanded provider conformance, malicious plugin tests |
| Phase 6 | Existing Sentinel laboratory sources retained | Live-host fleet coverage, consent/privacy proof, recovery drills, HA and measured SLOs |

Run [the Windows validation script](../tools/verify_tui_phase1.ps1) from a disposable checkout; capture commit SHA, exact binary SHA-256, commands, stdout/stderr and host OS details. No automated job or local assertion is currently evidence of a passed release. The 5-million-case corpus is a generated corpus, not five million executed agent scenarios.

## Execution rules

1. No weakening of operator ceilings, signed policy enforcement, audit-before-effects, data egress policy, approval separation or fail-closed sandbox selection for UI convenience.
2. Scope each phase to a user-observable outcome with a versioned contract, an owner, measurable acceptance criteria, a negative-test suite and rollback.
3. Never call an agent response, synthetic generator, a mock-provider green result or a source-only build a verified real-world completion.
4. Use a dedicated test worktree and disposable execution environment for any commands invoking untrusted repository code.
5. No use of a new release artifact until its target-specific build, smoke test, integrity check and platform isolation statements are independently verified.
6. Prefer a single accountable agent runtime. External Hivewire may coordinate work but cannot widen any Jani authority.

## Phase 0 — Product truth and traceability (foundation)

**Deliver:** clear README and navigation that leads with the coding agent; realistic scope and validation caveats; this programme; exact owner and per-gate evidence registry.

**Acceptance:** README distinguishes shipped features from experimental subsystems, links to validation, and includes a useful first-run read-only path. Documentation checker and claims checker pass. No credential leaks or invented release promises.

**Rollback:** revert documentation only. **Status:** partly addressed by documentation changes in the initial implementation slice; check commits and gates before closing.

## Phase 1 — Daily terminal experience (highest near-term value)

**Deliver:** a compact, legible, adaptive terminal; context-aware start actions that *prefill drafts without executing*; clear active model/worktree/scopes; unambiguous isolation labels; safe command palette and approval visibility. Follow with real multiline input, accessible keyboard navigation, diff views and structured tool timelines.

**Acceptance:**
- Starter shortcuts never execute tools, submit prompts or widen authority; Enter remains the sole send action.
- The welcome screen remains readable at 40x10, 80x24 and 120x35; normal conversation and scrollback are not altered.
- Actual OS isolation status must not be inferred from a configured sandbox *profile*.
- Test model picker, streaming, Unicode, clipboard/paste, TUI snapshots and Windows-console probe.
- For multiline input, preserve newlines in submitted text, visibly render cursor/wrapped lines, bound prompt size and test Unicode and paste.

**Evidence required:** Cargo fmt, clippy, workspace tests, terminal snapshots, Windows console probe and manual terminal pass. **Status:** starter-draft and clarity slice targeted in this commit series; the remainder is open.

## Phase 2 — Interruptible, durable, recoverable execution (release blocker)

**Deliver:** real cancellation with a cooperative token and deadline; transactional intent/outcome journal; checkpoints across completed turns; per-task fences; replay-safe tool IDs and reconciliation of ambiguous external side effects. Define state machine: pending -> authorised -> running -> verifying -> complete/partial/failed/cancelled/unknown.

**Acceptance:** inject kill -9/process termination during file write, network response and journal commit; recover without double execution or silently losing a side effect. Ctrl+C must either interrupt with a truthful result or explicitly say exit is deferred. "Conversation rolled back" must never imply that filesystem/network effects were undone. Verify idempotency keys, bounded retry and lease expiry.

**Rollback:** feature flag + migration-safe restore of pre-phase state. **Dependencies:** Phase 1 observation and existing broker/journal invariants.

## Phase 3 — Verification by evidence, not model assertion (release blocker)

**Deliver:** acceptance-contract compiler, independent verifier, structured tests/diffs/claims receipt, signed or externally anchored receipt design and human-review boundary. A model cannot mark a task complete based solely on its own words.

**Acceptance:** success requires matching acceptance predicates and inspectable evidence; unexecuted tests, stale hashes, altered result paths, forged approvals and missing receipts fail closed or are reported *unverified*. Verify via independent run and hostile test fixtures.

**Rollback:** disable automated completion labeling while retaining evidence records. **Dependencies:** Phase 2.

## Phase 4 — Security portability and supply chain (enterprise prerequisite)

**Deliver:** Windows/macOS confinement strategies tested against actual host OS policy, hardened Linux backend, secret/credential broker, signed provenance/SBOM, managed key revocation and policy distribution.

**Acceptance:** demonstrate filesystem/network escape rejection on each advertised OS, secret non-inheritance, compromised-key revocation, reproducible binary digest, installer refusal of tampering and explicit lack of support where isolation cannot be guaranteed. Maintain regression tests for default-deny, scope narrowing and network restrictions.

**Rollback:** remain on a prior validated artifact; unverified backends never silently downgrade.

## Phase 5 — Extensible execution and orchestration

**Deliver:** versioned, signed capability API and schema conformance; sandboxed WASM/sidecar plugin host; mediated MCP invocations, redacted logs, rate/concurrency controls; provider and model interoperability suites; Hivewire adapter for external multi-agent coordination.

**Acceptance:** hostile-plugin tests prove no direct filesystem/network/credential escape; grants bind task, tenant, principal and expiration; results preserve source authorization; human-only final commits remain human-only. Test offline providers, provider failover decisions and predictable budgets.

**Dependencies:** Phases 2–4. **Rollback:** disable affected adapter with no new authority.

## Phase 6 — Operated service and Sentinel (separate tracks)

**Jani service:** encrypted multi-tenant durable storage, lifecycle administration, incident handling, backup/restore drills, key rotation, monitoring, SLOs and regional failover.

**Sentinel:** independent host probe/gateway mTLS, discovered-service log coverage, normalized metrics, replay-safe spool, consent/privacy minimization and deployment/revocation operations. Do not treat a Sentinel rollout as part of agent maturity.

**Acceptance:** documented owner/tenant isolation, privacy and retention evidence, outage/chaos and soak tests, restore drills and measurable SLOs. No "all services covered" claim without inventory/adapter reconciliation.

## Phase gates and mandatory scorecard

| Gate | Proof, not a promise | Blocker if absent |
| --- | --- | --- |
| G0 Foundation | README/claims/doc indexes and reproducible baseline | Contradictory docs, unreproduced results |
| G1 UX | terminal sizes + shortcut tests + real Windows console screenshot/probe | Tool launched by starter key, misleading sandbox status |
| G2 Durability | crash-injection + replay receipts + cancellation outcomes | Duplicate/unknown external mutation hidden |
| G3 Verification | independent assertions + verified completion receipt | Agent self-attestation only |
| G4 Security | platform-specific isolation red-team + signed releases | Windows/macOS isolation assumed |
| G5 Extensions | malicious plugin/agent delegation conformance | Plugin bypasses Rust mediation |
| G6 Operations | retention, HA, recovery, privacy, on-call drills | Unable to restore or account for loss |

Track per phase: successful task outcome rate (externally checked), false completion rate, mean intervention count, time-to-first-useful-result, cost per verified change, undo/recovery reliability, denied-action correctness, cross-tenant leakage, regression escape counts and release incident rate. Report confidence intervals and denominators rather than fictitious targets.

## Initial implementation slicing

1. **Slice 0:** this phase plan, first-page product messaging and doc navigation.
2. **Slice 1A:** read-only starter prompts mapped to F1–F4 and honest sandbox-*profile* labelling in the existing ratatui TUI; update TUI tests and console probe expectation.
3. **Slice 1B (partially implemented in source):** bounded multiline composer, Shift+Enter, newline-preserving paste, tool result indicators and an audit-only completion summary. Explicit effective-permission inspection, rich diff preview and accessibility review remain open. Verify with [the Windows gate script](../tools/verify_tui_phase1.ps1). No new executable has been published.
4. **Slice 2A (in source; verification outstanding):** an atomic cancellation token is observed at safe execution boundaries with an auditable record. Ctrl+C no longer means deferred exit while busy. Blocking requests and already-running commands remain non-preemptible. Durable checkpoints, fencing and idempotent effect reconciliation are still absent. Failed-turn context now restores the exact pre-turn message snapshot even when input-history trimming has removed older turns; this is conversation state only, not rollback of prior tool effects. A preliminary correction to TUI/line-mode failure notices now makes clear that model-context rollback cannot reverse earlier tool effects.
5. **Slice 3A (in source, validation outstanding):** a journal-verified, audit-only `jani evidence RUN_ID` receipt can be generated after process exit without model credentials. It sets `outcomeVerified: false` and requires a future independent acceptance verifier before any stronger claim.
6. Subsequent slices enter only when preceding gate evidence exists and are independently reviewed.

**Change discipline:** each slice has a before/after SHA, explicit test ledger, and an honest unpassed-gates record. Publishing a binary requires a separate verified release procedure.
