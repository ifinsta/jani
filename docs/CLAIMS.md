# Documentation claims and source-conformance audit

Jani audits Markdown against current repository evidence using the standalone Rust tool at `tools/jani-claims/`. It is designed to **flag discrepancies, not invent evidence**.

## Running the auditor

```bash
cargo run -p jani-claims -- scan   # inspect findings without changing files
cargo run -p jani-claims -- sync   # refresh claims/index.json and claims/OPEN.md
cargo run -p jani-claims -- check  # fail if the tracked claims report is stale
cargo run -p jani-claims -- strict # also fail if open deterministic findings exist
cargo run -p jani-claims -- queue  # emit a bounded AI-review task list as JSON
```

The standard CI check uses `check`: changes to source or documentation that introduce, resolve, or move discrepancies must update the committed claims register. Existing open issues are visible but do not block the entire Rust workspace until triaged. Use `strict` when the backlog is cleared.

## Machine-checkable documentation assertions

Put a JSON assertion immediately below a factual statement in a Markdown file:

```md
Jani checks whether an MCP operation requires user-only submission.
<!-- jani-claim: {"id":"MCP-COMMIT","kind":"source_contains","path":"backend/mcp/src/lib.rs","needle":"USER_COMMIT_REQUIRED"} -->
```

The supported kinds are `source_contains`, `source_absent` and `file_exists`. Paths are always **root-relative** and restricted to the repository. A mismatch goes to `claims/OPEN.md` and `claims/index.json` with the document line and source path. The checker also flags broken local Markdown links and missing root-relative implementation path references. Fenced code is ignored. Forward-looking proposals are not treated as implementation promises by the path heuristic, but their links and explicit assertions are still checked.

This cannot prove arbitrary natural-language truth: presence of a symbol does not prove its security or runtime behavior. Use executable conformance tests for high-risk claims, and require a reviewer to inspect semantics.

## Optional AI-agent review

1. Generate a short task queue with `cargo run -p jani-claims -- queue > claims/review-queue.json` (review-queue.json is intentionally not a required CI artifact).
2. Assign a Jani agent **read-only** access to the queue, each cited Markdown file, and the relevant current Rust source. Run its model calls through Jani's existing signed policy and data-egress protections. Do not treat the agent as the authority for implementation changes.
3. Have the agent write a JSON report under `claims/agent-reviews/`, using the schema in [claims/README.md](../claims/README.md). Each candidate must cite an exact Markdown line and an exact quote present in a source file.
4. Run `jani-claims sync`; accepted citations appear in `claims/OPEN.md` as `needs_human_review`. Stale or forged references are rejected. No review automatically changes production code or marks a statement verified.

The queue and agent review inputs are **untrusted**. Their citations and file contents must pass Jani's normal data policy before being sent to an external model. Offline deterministic audits incur zero AI calls.

## Audit boundaries

The first version does not parse Rust ASTs, execute tests, establish causal guarantees, or perform complete semantic consistency checking. It enforces explicit evidence anchors and repository-reference consistency and provides a verifiable interface for higher-quality AI-assisted critique. Reports are reproducible and intentionally contain no timestamps, so ordinary CI diffing is stable.
