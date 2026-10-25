# Implementation review — enterprise proposal pack

**Reviewed:** 2026-10-25  
**Baseline:** `main` after the trusted vertical-slice implementation  
**Scope reviewed:** proposals 01–05 against the then-current Rust implementation  
**Verdict:** the proposals are **partially implemented** as a trusted local kernel.
They are **not** implemented at enterprise-pilot or supportable-release depth.

This document distinguishes what the binary actually enforces from what the
proposals describe. A feature is listed as *implemented* only when Rust enforces
it and a test or executable path demonstrates it.

**Historical review notice:** This document captures the initial trusted-kernel review, not the latest full baseline after MCP, native tools and attention enhancements. Consult [the current documentation index](../INDEX.md), [MCP governance](../MCP_GOVERNANCE.md), [native tools](../NATIVE_TOOLS.md) and [release acceptance](../RELEASE_ACCEPTANCE.md). Requirement statuses below require fresh verification against the current main branch before release sign-off.

## 1. Summary

Implemented end to end:

- Signed organization policy as a deny-first policy decision point (PDP), with the
  operator's CLI flags acting as a ceiling and work contracts able to narrow but
  never widen authority.
- Signed identity assertions bound to tenant and principal, and a model gateway
  that is the only component holding provider credentials.
- Data classification, secret/PII detection and redaction, and a fail-closed model
  egress matrix with per-request data envelopes.
- A tool broker that validates every tool call, enforces policy, contract
  capabilities, write scopes, budgets and approvals, and journals before and after
  execution.
- A hash-chained audit journal with verification, run inspection and fail-closed
  behavior when a record cannot be written.
- Persisted task budget ledgers covering tool calls, tokens, wall time and USD.
- Single-use approvals with exact capability/resource/payload binding and
  separation of duties.
- OS sandbox selection with no silent fallback: bubblewrap on Linux, a Windows Job
  Object on Windows, explicit refusal when a profile requires confinement that the
  host cannot provide.
- A signed plugin manifest registry with capability cataloguing. Plugins are **not
  executable** in this build.
- A fake-provider end-to-end test that runs the real CLI through policy, gateway,
  broker, journal and budget accounting.

Not implemented (the proposals' largest remaining areas):

- IdP/SCIM, role claims, ABAC and policy administration.
- Signed work contracts, execution grants and execution receipts.
- WASM/sidecar/MCP plugin execution and connector/model-provider adapters.
- Encrypted multi-tenant storage, retention/erasure and lineage persistence.
- Checkpoints, task leases, crash recovery and offline/outage operation.
- Filesystem and network confinement on Windows, and any macOS backend.
- Incident lifecycle, dual-control approvals and approval notification.
- A canonical JSON format and issuer/signing CLI for policies, identity assertions
  and plugin manifests. Signing helpers are test-only, and signatures cover this
  binary's `serde_json` field order rather than a standards-based canonical form.

## 2. Requirement traceability

### 2.1 Proposal 01 — first-class sandbox

| Requirement | Status | Evidence / limitation |
|---|---|---|
| Mandatory isolation for enterprise profiles | Partial | `sandbox::prepare` refuses isolation-required profiles when no backend exists and enterprise mode defaults to `Auto`. Windows cannot satisfy `EnterpriseRestricted`. |
| No silent fallback to unconfined execution | Implemented | `SandboxMode::Off`/`Auto` return errors for isolation profiles; `unconfined` is used only for non-isolation profiles. |
| Linux isolation | Partial | bubblewrap plan with PID/IPC/UTS/cgroup namespaces, `--die-with-parent`, `--clearenv`, read-only worktree and read-write approved scopes. No seccomp profile, no image digest. |
| Windows isolation | Not met | Job Object bounds the process tree only; `filesystem_confined` and `network_confined` are reported `false` and `EnterpriseRestricted` is refused. |
| macOS isolation | Not met | No backend; isolation profiles are refused. |
| Network policy | Partial | bubblewrap uses `--unshare-net` for `deny` and `loopback` (loopback is treated as deny), host networking only when explicitly selected. No per-destination allowlist. |
| Signed sandbox grant and attestation | Not met | Receipts are generated in-process and journaled by ID; they are not signed, not bound to a policy revision digest, and there is no TPM/attestation. |
| Trusted patching / staged application | Partial | Writes are atomic and scope-checked; there is no independent verifier or patch staging protocol. |
| Escape and lifecycle tests | Partial | Unit coverage for refusal/receipt honesty and process-tree bounding; no container escape suite, no crash-recovery suite. |

### 2.2 Proposal 02 — built-in data governance

| Requirement | Status | Evidence / limitation |
|---|---|---|
| Classification labels | Partial | `Classification` ordering; source default; email/SSN/card detection (Luhn-checked); secret detection by regex. No configurable classifiers, no document-level labels from a DLP service. |
| Secret redaction | Implemented | Secrets are detected and redacted before model egress. With redaction disabled, secret-classified content is still refused egress. Detection runs on decoded JSON string values, not escaped JSON. |
| Model provider allowlist | Implemented (fail-closed) | Empty `allowedProviders`/`allowedRegions`/`allowedPurposes` deny all egress; `"*"` is the only wildcard. Provider is derived from the API host; class and region are operator labels, not attested facts. |
| Classification egress ceiling | Implemented | Per-envelope class checks; restricted data requires an explicit exception; secret data is never permitted. |
| Tenant isolation in the gateway | Implemented at envelope level | Cross-tenant envelopes are rejected. State files themselves are not encrypted or tenant-partitioned. |
| Purpose limitation | Implemented | Purpose allowlist and per-envelope purpose must match the active task purpose. |
| Output inspection | Partial | Secrets are redacted; when `inspectOutputs` is enabled, RESTRICTED model output is rejected rather than returned to the loop. Tool results are classified and redacted by the broker. No full DLP, no output-to-input taint tracking. |
| Lineage and retention | Not met | Envelopes carry an empty `lineage`; no retention, erasure or legal-hold implementation. |
| Encrypted storage and key management | Not met | Journal, approvals and budget ledgers are plaintext JSON in `--state-dir`. |
| Provider-region attestation | Not met | Region is a CLI/env label. No provider contract, certificate or residency attestation is checked. |

### 2.3 Proposal 03 — organizational policy and governance

| Requirement | Status | Evidence / limitation |
|---|---|---|
| Signed policy bundle | Implemented | Ed25519 verification against trusted keys; tenant binding; expiry; `defaults.action` must remain `deny`. |
| PDP/PEP separation | Partial | `PolicyEngine::evaluate` is the PDP and the broker is the PEP. Both live in the same process; there is no remote PDP or signed decision token. |
| Default deny | Implemented | No matching signed rule denies. Operator flags are checked before policy rules and can only narrow. |
| Explicit denies | Implemented | `deniedActions` globs are evaluated before permits. |
| Roles / ABAC | Not met | Identity assertions carry no roles. Role-scoped policy rules are now **rejected at load** and can never match, so a policy cannot silently grant role-only authority. |
| Work contracts | Partial | Contracts are loaded, bound to verified identity, and can only restrict capabilities, write paths, sandbox profile and budgets. They are not signed and no canonical contract digest is journaled. |
| Contract capability semantics | Implemented | When a contract is present, its capability list and write paths are authoritative; omitting them grants nothing. Path traversal and absolute paths are refused. |
| Approvals | Partial | Exact binding and single use are enforced; requester cannot self-approve; TTL is enforced. `--role` is caller-asserted rather than derived from a signed identity claim; there is no dual control or notification. |
| Budget authority | Implemented locally | Tool-call, wall-clock and token budgets, persisted across restarts, with limit tightening on reopen. USD is priced from the policy and checked before calls. |
| Audit journal | Partial | SHA-256 hash chain, verification, tamper test, fail-closed write path. The chain is not signed or externally anchored; there is no WORM storage or retention policy. |
| Identity lifecycle | Not met | No IdP, SCIM, revocation list, key rotation, audience restriction or nonce replay cache. |
| Policy administration | Not met | No admin API, staged rollout, rollback or four-eyes policy approval. |
| Incident handling | Not met | No incident state machine, containment actions or notification pipeline. |
| Offline/outage behavior | Not met | `policyOutage` is parsed but unused. |
| Checkpoints and leases | Not met | No `task-checkpoint`, no lease, no recovery loop. |
| Managed deployment | Not met | Single process, local state directory, no HA or distributed journal. |

### 2.4 Proposal 04 — plugin architecture

| Requirement | Status | Evidence / limitation |
|---|---|---|
| Versioned manifest | Implemented | `PluginManifest` with API version, publisher, runtime, capabilities and requested permissions. |
| Signature and digest verification | Implemented | Manifest digest excludes `integrity`; Ed25519 signature verified against `--plugin-key`; unsigned manifests, bad digests, duplicate IDs and malformed identifiers are refused. |
| Capability catalog | Implemented | Namespaced `plugin/capability@version`; duplicate capability names refused; input schema must be an object. |
| Path and size hygiene | Implemented | Symlinked manifests refused; 128-manifest and 512 KB limits. |
| Native dynamic loading | Refused | Only `builtin`, `wasm` and `sidecar` runtime labels are accepted; native loading is rejected. |
| Plugin execution | Not met | Registry output explicitly reports `invocable: false`; there is no WASM runtime, sidecar supervisor or MCP client. |
| Broker mediation for plugins | Not met | No plugin-originated call path exists yet. |
| Supply-chain controls | Partial | Signature, digest and publisher fields exist; no revocation, transparency log, SBOM or provenance attestation. |
| SDK | Not met | No plugin SDK or ABI. |

### 2.5 Proposal 05 — gates

| Gate | Status | Reason |
|---|---|---|
| G0 — reliable current agent | Partial | Build, lint, unit/CLI/E2E tests and release build pass on Windows. No live-provider session was run. |
| G1 — trusted local kernel | Partial | PDP/PEP, DGK, broker, journal, budgets and approvals exist. Identity roles, signed grants/receipts and platform confinement gaps remain. |
| G2 — safe autonomous code execution | Not met | Windows cannot provide filesystem/network confinement; no macOS; no independent verifier; no checkpointed recovery. |
| G3 — governed extensibility | Not met | Manifests are verified but plugins cannot execute. |
| G4 — enterprise pilot | Not met | No IdP/SCIM, policy administration, encrypted tenant storage, HA, incident handling or offline mode. |
| G5 — enterprise supportable release | Not met | No support SLOs, migrations, rollout/rollback, or operational runbooks beyond this review. |

## 3. Fail-closed behavior audit

The implementation was reviewed specifically for places where an omitted or
malformed input could silently grant authority. These are now closed:

| Risk | Behavior | Test |
|---|---|---|
| Policy rule scoped by roles | Load fails; evaluation treats such a rule as non-matching | `role_scoped_rules_are_rejected_and_never_match` |
| Empty provider/region/purpose allowlist | Denies egress; only explicit `"*"` is permissive | `egress_allowlists_fail_closed_and_accept_only_explicit_wildcards` |
| Work contract omits capabilities or write paths | Grants nothing | `a_contract_that_omits_capabilities_grants_nothing` |
| Secret content with redaction disabled | Refused egress, never transmitted | `secret_never_leaves_even_when_redaction_is_off` |
| Audit record cannot be written | Call is denied and the error is surfaced | broker audit error path (`broker::Broker::invoke`) |
| Malformed or unknown tool call | Journaled denial before any action | `malformed_and_unknown_tool_requests_are_audited_and_denied` |
| Exhausted budget | Denied and journaled | `exhausted_budget_denies_and_audits` |
| Corrupt or tightened budget ledger | Fails closed or adopts the stricter limit | `corrupt_ledgers_fail_closed`, `reopening_a_ledger_preserves_usage_and_tighter_limits` |
| Sandbox backend missing for an isolation profile | Error, no unconfined fallback | `isolation_profiles_never_fall_back_to_unconfined`, `auto_mode_refuses_enterprise_profile_without_backend_or_confines` |
| Windows Job Object claimed as filesystem confinement | Explicitly reports no filesystem/network confinement | `windows_job_does_not_claim_filesystem_confinement`, `receipts_report_network_confinement_consistently` |
| Unsigned or tampered plugin manifest | Registry load fails | plugin registry tests |
| Approval reused or rebound | Consume fails | `approval_binds_exact_payload_and_is_single_use` |
| Provider credentials in a child process | Stripped before spawn | `children_do_not_inherit_provider_credentials` |

Residual fail-open risks that remain by design or omission:

1. **Caller-asserted approval role.** `--role` is not tied to a signed identity
   claim, so an operator who can run the CLI can name the required role.
2. **Unsigned work contract.** A local file can be edited between validation and
   use; only the operator's own authority is at stake, but there is no integrity
   evidence.
3. **Unattested provider location.** `--provider-region` and `--provider-class`
   are labels, not verified provider properties.
4. **Local audit chain.** Tampering is detectable but not prevented, and the
   chain can be deleted with the state directory.
5. **Windows execution.** A Job Object does not prevent a child from reading or
   writing arbitrary files, and enterprise profiles are therefore refused rather
   than falsely satisfied.
6. **Signature interoperability.** Policy, identity and plugin signatures cover
   Rust-struct-ordered compact JSON, not RFC 8785/JCS. There is no issuer CLI, so
   an external IdP or policy service cannot reliably produce a bundle this binary
   will accept. See `docs/schemas/README.md`.

## 4. Verification evidence

Run on Windows with the stable Rust 1.95.0 toolchain
(`x86_64-pc-windows-msvc`):

```text
cargo fmt --all -- --check        pass
cargo clippy --all-targets -- -D warnings   pass
cargo test --all-targets          61 unit + 5 CLI + 1 E2E, all pass
cargo build --release             pass
```

The E2E test (`tests/e2e.rs`) starts a fake OpenAI-compatible provider, runs the
real `jani` binary against a temporary worktree and state directory, and asserts:

- the final answer and tool call complete through the normal loop;
- the provider request never contains the API key;
- two provider requests are made with the tool result included;
- the journal contains an executed `repo.read` and a completed `agent.run`;
- `audit verify` reports an intact chain;
- the persisted budget ledger records one tool call and the provider token usage.

Not verified:

- a live provider or a real coding session;
- Linux bubblewrap execution or the `cfg(unix)` test path;
- macOS;
- sandbox escape resistance under an adversarial container;
- crash/restart recovery of an interrupted run;
- multi-tenant isolation of state files.

## 5. Recommended order of work

1. **Signed execution evidence:** canonical work-contract digest, signed execution
   grant, signed tool receipt, and journal fields binding them to the policy
   revision.
2. **Roles and approval authority:** role claims in the identity assertion, policy
   role rules, approval roles derived from verified identity, dual control for
   high-risk classes, and approval notification/inspection of the exact payload.
3. **Issuer and IdP integration:** a canonical JSON form (RFC 8785/JCS), signing
   and verification CLI, OIDC/SCIM ingestion, key rotation and revocation,
   audience and issuer allowlists, and a nonce replay cache.
4. **Platform confinement:** a Windows AppContainer or VM backend that can satisfy
   `EnterpriseRestricted`, a macOS backend, and a documented support matrix.
   Until then, enterprise use on Windows must remain refused for isolation
   profiles.
5. **Plugin execution:** a WASM/sidecar executor behind the broker and sandbox,
   plus MCP mediation and per-capability grants.
6. **Tenant-grade storage:** encrypted, tenant-partitioned journal/approvals/budget
   storage, retention/erasure, and persisted lineage.
7. **Operations:** policy administration, offline leases, checkpoints and recovery,
   incident handling, and CI coverage on Linux and macOS.

## 6. Bottom line

This build is a credible **trusted local kernel vertical slice** with unusually
careful fail-closed behavior. It is suitable for supervised local evaluation with
disposable checkouts. It is not an enterprise-ready implementation of the proposal
pack: the identity/role control plane, plugin execution model, cross-platform
confinement, signed execution evidence and tenant-grade storage are still
missing. Treat G4 and G5 as unimplemented, and do not describe this build as
production approved.
