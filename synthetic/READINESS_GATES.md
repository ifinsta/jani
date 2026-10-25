# Jani production-readiness evidence matrix

**Status: NOT CERTIFIED.** This document is an acceptance contract for a future
test campaign. It makes no claims that any 5M synthetic record has executed.

## Hard blockers (zero tolerance)

- Any unauthorized read, cross-tenant disclosure, forged/delegated permission
  accepted, or use of stale/revoked rights.
- Any agent-only execution of initiating-user-only commits.
- Any secret exposed to an unapproved external model/provider or stored in an
  untrusted tool response without classification.
- Any uncontrolled external side effect, duplicate irreversible action,
  or automatic retry after ambiguous external outcomes.
- Any unconfined execution reported as successfully sandboxed.
- Any tampered or missing authoritative audit event treated as a verified success.
- Any critical security failure, unsupported mandatory claim, or regression not
  explicitly investigated and independently verified.

## Fault campaigns mapped to code

| Domain | Target source | Mandatory runtime evidence |
| --- | --- | --- |
| Identity | backend/core/src/identity.rs | issuer/tenant/subject/time/signature rejection |
| Delegation | backend/mcp/src/lib.rs | least privilege, expiry, revocation and no role spoofing |
| Policy | backend/core/src/policy.rs | deny precedence, permission scope and revalidation |
| MCP | backend/server/src/mcp.rs | audience-bound token, response receipt, human-only commit refusal |
| Data governance | backend/core/src/data.rs | classification inheritance, redaction and permitted egress |
| Model gateway | backend/server/src/gateway.rs | safe retries, quotas, malformed responses and secret non-disclosure |
| Tool broker | backend/server/src/broker.rs | unknown tools denied, source auditing before effects |
| Workspace | backend/server/src/workspace.rs | symlink/hardlink/traversal/TOCTOU protection |
| Sandbox | backend/server/src/sandbox.rs | real platform confinement or explicit fail-closed |
| Process | backend/core/src/process.rs | bounded execution, process trees, cancellation and output caps |
| Plugins | backend/server/src/plugins.rs | signed manifest and no authority escalation |
| Budget | backend/core/src/budget.rs | restart-safe, race-safe and stricter limit enforcement |
| Approvals | backend/core/src/approvals.rs | exact payload, single use and separation of duties |
| Audit | backend/core/src/journal.rs | hash chain, tamper detection and crash reconciliation |
| Storage | backend/store/src/lib.rs | migrations, crash consistency and durable outbox |
| Lifecycle | backend/server/src/attention.rs | allowed transitions, quarantine and checkpoint fencing |
| Alerts | backend/server/src/attention.rs | idempotency, leased ownership, ack/retry and wake policy |
| Mirror | backend/mirror/src/lib.rs | citation-only critique and no false success |
| Claims | tools/jani-claims/src/main.rs | deterministic source checks, stale AI evidence and review gates |
| Contracts | backend/core/src/contract.rs | unknown fields, versions and illegal transitions rejected |
| CLI | backend/core/src/config.rs | invalid flags, env overrides, no secret echo |
| Operations | backend/server/src/main.rs | Linux/Windows platform differences, outage/rollback and resource exhaustion |
| Cross-system | multiple sources | revocation mid-operation, crash after effect and policy changes |

## Execution levels

| Level | Test scope | Evidence required |
| --- | --- | --- |
| L0 | All 5M scenario generation and checksum verification | Manifest with SHA-256 per shard, row counts, deterministic replay |
| L1 | Workspace Rust unit/integration tests | Commit, toolchain/OS, immutable logs, assertion counts |
| L2 | Fake IdP/provider/MCP executable integrations | Verified token scopes, policy receipts, record-level filtering, denied and permitted cases |
| L3 | Fault injection in disposable OS/container environments | Exact hook/phase, before/after state, checkpoint and recovery receipts |
| L4 | Concurrent full-corpus replay, extended soak and load | p50/p95/p99 latency, peak memory, CPU, disk/IO, error rate and throughput |
| L5 | Adversarial external security review and disaster recovery | Signed review evidence, independent remediation verification, backup restoration drill |

For metrics such as p99 latency, CPU utilization and recovery-point/recovery-time
objectives, define application workload-specific acceptance thresholds **before**
running the campaign. Do not backfill targets after seeing the result.

Every case needs an actual adapter: the current runner supports four safe CLI
preflight hooks only. Unsupported cases must remain SKIPPED; never count skipped
scenarios as successes. The full corpus currently reports
\`replay.executed=false\` for all 5M rows.

## Release blockers and receipts

The 16 independent release gates are defined in \`synthetic/readiness.py\`.
Results must record source commit, host/OS, immutable fixture digest,
execution command/hook, actual observed effect, predicted effect, verdict,
retries, denial/approval/audit evidence, test environment and trace artifacts.

External effects must be executed against controlled doubles unless specifically
authorized in an isolated staging environment. No real banking operations,
customer accounts, live OAuth tokens or production secrets are permitted in
the generated corpus.

**The readiness evaluator fails with exit code 2 when evidence is missing.**
It currently checks receipt metadata, not cryptographic attestation; independent
CI integrity controls and reviewer signoff are also required before a
regulated enterprise release can be accepted.
