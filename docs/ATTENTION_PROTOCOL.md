# Jani native lifecycle and attention protocol v1 (standalone)

Status: initial implementation for **one trusted local machine**. Not an enterprise-ready event broker, remote queue, or autonomous scheduled execution service.

## Design invariants

- Jani is independently executable. No Hivewire package, command, schema, API or transport is required or imported.
- Future coordinators can invoke Jani's stable CLI and/or an optional adapter. Coordination must not be hardcoded into this runtime.
- External alert content is *data*, not instructions. This initial implementation deliberately stores only an event name, source, reference, severity and idempotency key, never free-form executable prompts or embedded payment data.
- An alert is **not** a capability grant. Identity and signed policy are reevaluated on every command in enterprise mode.
- A wake hint is not authentication, an acknowledgement, an instruction, or verified task completion.
- The alert store represents delivery and triage state, not an authoritative banking transaction ledger.
- No automatic financial execution, plugin execution, model restart, or completed-work claims are added by this iteration.

## Architecture

\`\`\`text
  trusted source / local administrator
         | attention.alerts.send (policy checked)
         v
  .jani/attention/state.json  <-- OS-locked, atomic rewrite
         |
         v
  wake poll / wake watch --> lifecycle sleeping -> ready
         |
         v
  verified agent identity --> alert claim (60s lease)
         |
         +--> alert accept (responsibility recorded)
         |         |
         |         +--> external governed task + verification
         |                        |
         |                        +--> alert resolve (evidence ref)
         |
         +--> alert retry (bounded exponential delay)
\`\`\`

The watcher is a low-cost Rust process that polls durable local state every two seconds. It only moves a sleeping agent to READY when an eligible high/critical alert has set the wake flag. The watcher does **not** launch a model or issue commands on behalf of the worker. Model invocation remains an explicit, governed \`jani run\` operation. This separation avoids treating message delivery as permission to execute.

## Three distinct states

1. **Agent phase**: ready, running (reserved for future supervised execution), sleeping, paused, quarantined. The current CLI permits register -> ready, ready -> sleeping, and sleeping -> ready. It never lifts pause or quarantine.
2. **Task state**: remains owned by the existing run/budget/audit system or a future optional orchestrator. This implementation does not fabricate work checkpoints or declare task completion.
3. **Alert phase**: pending, leased, accepted, resolved. A resolved alert carries a reference to external evidence; Jani does not treat that reference as independently verified proof.

Only ready/running agents may claim an alert. A low-severity alert remains queued and does not wake a sleeping agent. A high/critical alert requests a wake without modifying quarantine/pause.

## CLI contract

\`\`\`bash
jani agent register worker-01 --state-dir .jani
jani agent status worker-01 --state-dir .jani
jani agent sleep worker-01 --state-dir .jani

jani alerts send worker-01 payment.failed banking case-42 high event-key-42 --state-dir .jani
jani wake poll worker-01 --state-dir .jani
jani wake once worker-01 --state-dir .jani
# Or run a lightweight wake supervisor, supervised externally:
jani wake watch worker-01 --state-dir .jani

jani alerts list worker-01 --state-dir .jani
jani alerts claim worker-01 --state-dir .jani
# Record the alert ID and receipt token returned by claim:
jani alerts accept worker-01 ALERT_ID RECEIPT --state-dir .jani
# After separate task investigation and receipt verification:
jani alerts resolve worker-01 ALERT_ID EVIDENCE_REF --state-dir .jani
# For a transient failure, instead of accepting:
jani alerts retry worker-01 ALERT_ID RECEIPT --state-dir .jani
\`\`\`

Use explicit \`--mode enterprise --policy SIGNED_POLICY --policy-key ISSUER_KEY --identity SIGNED_IDENTITY --tenant ... --principal ...\` to govern managed operation. Policies must permit the specific actions and resources:

\`\`\`text
attention.agent.register
attention.agent.status
attention.agent.sleep
attention.agent.wake
attention.alerts.send
attention.alerts.list
attention.alerts.claim
attention.alerts.accept
attention.alerts.retry
attention.alerts.resolve
attention.wake.poll
attention.wake.once
\`\`\`

For enterprise inbox operations, the authenticated principal must equal the recipient agent ID. Enterprise registration and publishing require a verified signed identity and matching signed policy permission. Broader administrator roles and delegated inbox reading require a future centrally verified role/relationship policy; they are intentionally **not** inferred from caller strings.

## Storage and guarantees

- The JSON snapshot is serialized under an operating-system file lock, available on Rust 1.89+. Lock ownership is released automatically when the process exits.
- Writes use a same-directory temporary file, fsync and rename; a failure refuses mutation rather than replacing an invalid snapshot with empty state.
- The local journal records authorized transition **intent** before snapshot mutation. Its record does not prove the mutation was committed. Because journal and snapshot are separate files, their updates are not an atomic distributed transaction.
- Consumer receipts include an opaque lease token and expiration; an expired or stale token cannot acknowledge a new delivery. A crash after claim permits redelivery following lease expiry.
- Published alerts are deduplicated on (tenant, sender, idempotency key). Reusing a key with different contents is refused; using the same key is replay-safe.
- Retries use bounded exponential delays up to five minutes, without resetting the attempt count.
- A maximum of 10,000 retained alerts and an 8 MB serialized-state ceiling bound the prototype's memory/disk use. There is no automatic pruning, database archiving, or infinite retention guarantee.
- File-lock behavior has **not been validated for network filesystems**. Do not share one attention store across hosts, run untrusted writers, or use this as a multi-tenant banking service.
- The JSON state remains locally plaintext. Place the state directory on an appropriately encrypted volume under OS filesystem ACLs.
- Only identifiers and external references are stored. Do not put account numbers, secrets, sensitive personal details or large payloads in these fields.

## Wake and action trust separation

The alert router is intentionally simple. The system **must not** generate an LLM prompt directly from an alert event payload and run it autonomously. A future action executor should:

1. Acquire an eligible inbox delivery under a fencing generation.
2. Restore a trusted task/checkpoint, not content supplied by the event sender.
3. Revalidate current signed policy, delegated rights, sandbox capability, data policy and budget.
4. Request permitted tools only through Jani's existing broker.
5. Persist action intent and external idempotency key before irreversible side effects.
6. Reconcile uncertain side effects using the authoritative external system.
7. Checkpoint and record evidence; separately acknowledge alert handling and task verification.

The existing Jani audit journal is not yet a multi-writer transaction log, and the current tool broker does not expose remote event actions. Do not claim fully automatic response or externally exactly-once execution.

## Future plugin surface (optional)

An external plugin or coordinator can provide event ingestion, advanced subscriptions, durable remote transport and supervision through an adapter. It must not acquire runtime write access or bypass enterprise policy simply by being installed.

Suggested extensibility API for a future version:

\`\`\`rust
pub trait AttentionSource {
    fn poll(&self, cursor: &str, limit: usize) -> Result<EventBatch, AttentionError>;
}
pub trait WakeNotifier {
    fn notify(&self, agent_id: &str, generation: u64) -> Result<(), AttentionError>;
}
\`\`\`

The traits above are **proposed**, not present in code. An adapter should submit validated alerts through a future authenticated local API, and Jani should retain independent policy and execution authority. No Hivewire dependency should be added to Cargo.toml.

## Required next release gates

- Add an authenticated broker-facing event ingestion API with verified principals and per-source quotas, replacing local CLI-only publishing in managed deployments.
- Implement real task checkpoints, owned process supervision and cancellation boundaries, distinct from alert delivery.
- Replace the single-file store with a transactional store supporting tenant partitioning, durable outbox, multi-writer audit, encryption, retention/erasure and cross-host delivery.
- Add dead-letter and escalation states plus externally corroborated resolution receipts.
- Integrate work contracts into wake execution; never restore stale agent privileges.
- Add fault injection for power loss after journal intent, after state rename, after external action, and before acknowledgement.
- Benchmark idle overhead, wake latency, duplicate-delivery rate and policy-decision time across supported operating systems.
- Validate security on Linux and Windows before any banking production pilot.
