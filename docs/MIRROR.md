# Jani Mirror — evidence-based reflection (v1)

**Implementation scope:** deterministic, local, low-cost reviews of existing Jani audit events. Mirror does **not** call an LLM, replay conversations, claim to observe private model reasoning, execute tools, or self-modify the agent.

## Purpose

Mirror reconstructs **what the trusted runtime actually recorded** and separates observation from inference. Its five goals are reconstruction, critique, comparative workflow review, proposed improvement and later evidence-based evaluation.

Architecture follows the ifinpay-style workspace:

\`\`\`text
backend/
  core/     policy, identity, hash-chained audit journal
  store/    transactional Mirror recommendation register
  mirror/   independent deterministic analyzer, no model SDK
  server/   CLI composition and authorization
contract/   existing generated lifecycle and alert states
tools/      architectural conformance checks
\`\`\`

Mirror deliberately has **no** Hivewire dependency. Future collaboration traces can be ingested only through an optional, governed adapter.

## Commands

\`\`\`bash
# All examples run from the Jani workspace root.
jani mirror inspect RUN_ID --state-dir .jani
jani mirror critique RUN_ID --state-dir .jani
jani mirror workflows --state-dir .jani
jani mirror recommendations --state-dir .jani
jani mirror recommendations RUN_ID --state-dir .jani
jani mirror compare RUN_A RUN_B --state-dir .jani

# Explicitly save proposed improvements, without changing any Jani workflow.
jani mirror record RUN_ID --state-dir .jani
jani mirror history --state-dir .jani
\`\`\`

For \`cargo run\`, pass command arguments after \`--\`.

Every Mirror read verifies the existing hash-chained \`journal.jsonl\` before reading it. An invalid or oversized (>32 MiB) journal is refused, not silently skipped. An absent journal produces an empty workflow summary; inspecting an unknown run returns an error.

Enterprise deployments require valid signed identity and an organization policy that **explicitly permits** \`mirror.inspect\`, \`mirror.critique\`, \`mirror.workflows\`, \`mirror.recommendations\`, \`mirror.compare\`, \`mirror.history\`, and (separately) \`mirror.record\`. Resources are \`mirror:run:RUN_ID\` or \`mirror:tenant:TENANT_ID\`. Reading never confers write permissions; \`record\` is a distinct policy-controlled mutation.

## How Mirror reconstructs a run

- Audit entries are filtered by the **verified tenant** and exact run ID.
- Only the trusted \`agent.run\` \`completed\` / \`failed\` event is considered terminal.
- Missing terminal evidence is \`incomplete\`, not success or failure.
- Timeline, action counts, decision counts, recorded duration and cited event IDs are returned.
- Timeline output is bounded to the last 160 events, with a truncation flag and true total event count.
- Prompts, file contents, arguments, tool results, API keys and resource digests are excluded. Unusual identifiers are redacted.
- The terminal time difference is **observed audit span**, not necessarily real wall-clock runtime.
- No unverifiable root cause, performance gain, model reasoning or work quality judgment is invented.

## Deterministic rules

| Rule | Trigger | Suggested remediation |
|---|---|---|
| \`run_incomplete\` | No trusted terminal run event | Inspect crash/termination and recovery evidence |
| \`failed_run\` | Trusted run failure | Inspect failure reason, receipt and model/provider context |
| \`step_budget_exhausted\` | Failure with \`step_limit\` reason | Evaluate a smaller task plan or controlled budget |
| \`repeated_denial\` | Same action and reason denied at least twice | Stop unauthorized retries, request approval or change plan |
| \`verification_evidence_gap\` | Recorded repository write and no permitted \`command.exec\` in same run | Locate any independent test evidence; do not claim untested work is verified |

Findings use stable deterministic IDs, cited audit event IDs, severity, confidence \`rule_based\` and status \`proposed\`. They are heuristic observations, not proof of the cause of a defect.

\`workflows\` samples the 50 most recent distinct audited agent runs and reports outcome distribution plus repeated finding types. These are descriptive counts, **not** causal workflow evaluations.

\`compare\` calculates event-count and denial-count deltas without pretending two unrelated tasks are comparable.

## Efficient review lifecycle

1. Run Jani normally; no additional logging or model inference is needed.
2. Start with \`jani mirror workflows\` to identify repeated friction.
3. Drill down with \`mirror critique RUN\`; fetch the bounded timeline only with \`mirror inspect RUN\`.
4. Use \`mirror record RUN\` **explicitly**, after reviewing the findings.
5. Review the durable \`mirror history\` register; proposed changes require a separate human-approved implementation process.
6. When a workflow changes, compare **matched** scenarios with tests/simulations. Mere changes in event count do not establish improvement.

\`backend/store\` persists registered suggestions in \`.jani/mirror/improvements.sqlite3\` (SQLite WAL + FULL synchronization). The register is tenant-scoped and deduplicated by stable recommendation ID. An idempotent re-record is safe, while a conflicting reuse of an existing ID is refused.

## Non-goals and future roadmap

- **No autonomous workflow rewriting or model self-approval.** Proposed improvements never become new privileges.
- **No simulated replay yet.** A separately built simulator should support matched counterfactual tests and fault injection.
- **No opinionated LLM critic yet.** This MVP deliberately avoids per-run inference costs. Optional higher-order critiques can come later with explicit budgets and data controls.
- **No external credential egress** from \`jani-mirror\`; only audited identifiers are serialized to the CLI.
- **No proven performance claim** without benchmark evidence.

### Operational constraints

Mirror currently refuses journals over 32 MiB and uses Jani's existing sequential JSONL hash-chain verification. It does not implement efficient indexed streaming at unbounded scale, audit multiwriter repair, historical redaction/erasure, remote storage, telemetry ingestion from tools that do not record evidence, or automatic matching of comparable tasks.

Deploy only within Jani's existing trusted state-directory security boundary. Validate backups and migrations before regulated production use.
