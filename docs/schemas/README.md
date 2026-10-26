# Canonical schemas — implementation status

**Status:** partial. These are the record shapes this build actually reads and
writes. Several schemas named in the proposal pack (`sandbox-grant`,
`task-checkpoint`, a signed `action-request`) do not exist yet.

**Source of truth:** `backend/core/src/types.rs`, `backend/core/src/policy.rs`, `backend/core/src/identity.rs`,
`backend/server/src/plugins.rs`, `backend/core/src/journal.rs`, `backend/core/src/approvals.rs`, `backend/core/src/data.rs`.

All externally supplied files are JSON. Field names are `camelCase` except audit
events, which are `snake_case`.

## 1. Implemented external file formats

| Proposal schema | Rust type | Where it is used | Signed? |
|---|---|---|---|
| `work-contract/v1alpha1` | `WorkContract` | `--contract`, `jani contract validate` | No |
| `plugin-manifest/v1alpha1` | `PluginManifest` | `--plugin-dir` | Ed25519 over the manifest digest |
| Organization policy (proposal 03) | `PolicyBundle` | `--policy` | Ed25519 over the bundle without `signature` |
| Identity assertion (proposal 03) | `IdentityAssertion` | `--identity` | Ed25519 over the assertion without `signature` |
| Project settings (not in the proposal table) | `Settings` | `.jani/settings.json`, `~/.jani/settings.json`, `--settings`, `JANI_SETTINGS`, `JANI_USER_SETTINGS` | No; an inline key is accepted but discouraged |

### 1.1 Work contract

See [work-contract.example.json](work-contract.example.json). It is accepted by
`jani contract validate`. A contract can only narrow the signed policy and the
operator's flags; omitting `capabilities` or `writePaths` grants nothing.

Required: `apiVersion`, `kind: "WorkContract"`, `metadata.taskId`,
`metadata.tenant`, `metadata.principal`; `spec.purpose`.

An unsigned contract has no integrity evidence. Verify the file out of band or
keep it under the operator's control.

### 1.2 Plugin manifest

Minimal shape:

```json
{
  "apiVersion": "plugins.jani.dev/v1alpha1",
  "kind": "Plugin",
  "metadata": {
    "id": "com.example.lookup",
    "name": "Lookup",
    "version": "1.0.0",
    "publisher": "example"
  },
  "spec": {
    "kind": "tool",
    "runtime": { "type": "wasm", "artifact": "lookup.wasm", "entrypoint": "lookup" },
    "capabilities": [
      {
        "name": "lookup",
        "version": 1,
        "effect": "read",
        "riskTier": "R0",
        "resources": ["repo:**"],
        "dataClasses": ["PUBLIC", "INTERNAL"]
      }
    ],
    "requestedPermissions": {
      "networkServices": [],
      "secrets": [],
      "dataPurposes": ["SOFTWARE_DEV"],
      "fsAccess": "none"
    }
  },
  "integrity": {
    "digest": "<sha256 hex of the manifest without integrity>",
    "signature": "<ed25519 hex over the digest bytes>",
    "keyId": "publisher-2026"
  }
}
```

`integrity` is mandatory for registry load. The digest is SHA-256 over the
compact JSON serialization of the manifest with `integrity` removed. Only
`builtin`, `wasm` and `sidecar` runtime labels are accepted; native dynamic
loading is refused. Manifests are verified, catalogued and reported as
`invocable: false`; this build has no plugin executor.

### 1.3 Organization policy

Minimal shape (signature value omitted):

```json
{
  "apiVersion": "jani.dev/v1alpha1",
  "kind": "OrganizationPolicy",
  "metadata": {
    "organization": "org-example",
    "revision": "pol-2026-10-25.1",
    "issuer": "security-team",
    "tenant": "org-example",
    "issuedAt": 1790000000,
    "expiresAt": 1790086400
  },
  "spec": {
    "defaults": {
      "action": "deny",
      "sandboxProfile": "enterprise-restricted",
      "network": "deny",
      "dataEgress": "deny",
      "audit": "required"
    },
    "rules": [
      {
        "action": "repo.read",
        "resource": "repo:src/**",
        "maxRisk": "R1",
        "effect": "read",
        "roles": []
      }
    ],
    "deniedActions": ["external.*", "command.exec"],
    "data": {
      "sourceDefault": "INTERNAL",
      "allowedModelClasses": ["PUBLIC", "INTERNAL", "CONFIDENTIAL"],
      "allowedProviders": ["api.example-llm.com"],
      "allowedRegions": ["eu-west-1"],
      "allowedPurposes": ["SOFTWARE_DEV"],
      "redactSecrets": true,
      "allowRestrictedEgress": false,
      "inspectOutputs": true
    },
    "budgets": {
      "maxToolCalls": 50,
      "maxWallSeconds": 1200,
      "maxUsd": 3.0,
      "pricing": { "usdPer1kInputTokens": 0.01, "usdPer1kOutputTokens": 0.03 }
    },
    "policyOutage": {
      "allowOfflineRead": true,
      "maxOfflineLeaseSeconds": 900,
      "allowOfflineExternalMutations": false
    }
  },
  "signature": { "keyId": "security-team-2026", "value": "<ed25519 hex>" }
}
```

Rules scoped by `roles` are **rejected** because an identity assertion carries no
roles. Empty `allowedProviders`, `allowedRegions` or `allowedPurposes` deny all
model egress; `"*"` is the only wildcard. `maxRisk` is `R0`–`R4`; `effect` is
`read`, `local_write`, `external_write`, `irreversible`, `model_invoke` or
`control`.

### 1.4 Identity assertion

```json
{
  "apiVersion": "jani.dev/v1alpha1",
  "kind": "IdentityAssertion",
  "principal": "human-82",
  "tenant": "org-example",
  "subjectType": "human",
  "issuer": "idp-example",
  "issuedAt": 1790000000,
  "expiresAt": 1790000600,
  "nonce": "<unique nonce>",
  "signature": { "keyId": "idp-2026", "value": "<ed25519 hex>" }
}
```

`principal` and `tenant` must match the CLI request. Expiry, future-issued
assertions and an empty nonce are rejected. The nonce is not replay-checked and
there is no audience field yet.

### 1.5 Settings

See [settings.example.json](settings.example.json) and the JSON Schema
[settings.schema.json](settings.schema.json). Settings supply named model
profiles, a model selector, transport settings, a `maxSteps` limit and
Claude-style `permissions` rules; they cannot grant write or command authority.
A worktree `.jani/settings.json` shadows the user-wide
`~/.jani/settings.json`; files are not merged.

Required: `version: 1`. If `models` is non-empty, `defaultModel` must name one of
them unless a top-level `model` selector is present. Each profile
requires `model` and may set `apiBase`, `apiKey`, `apiKeyEnv`, `protocol`
(`openai` or `anthropic`) and `maxOutputTokens`. `apiKeyEnv` names the
environment variable holding the credential. An inline `apiKey` is accepted for
explicitly configured convenience, but it is redacted from `Debug` and
diagnostics and should be replaced with `apiKeyEnv` in shared checkouts.
`permissions` accepts `defaultMode` (`default`, `acceptEdits`, `plan`; not
`bypassPermissions`), plus `allow`, `ask` and `deny` lists of `Read(./path)`,
`Write(./path)` and `Bash(command *)` rules. Rules narrow the operator's CLI
grants, work contract and signed policy; they never widen them. Unknown fields
fail closed. CLI flags override environment variables, which override settings.

## 2. Implemented internal records

These are generated by the runtime and written to `--state-dir` or returned to
the model. They are not independently versioned files.

| Proposal schema | Rust type | Persistence |
|---|---|---|
| `action-request/v1alpha1` | `ActionRequest` | In memory, passed to the PDP |
| `policy-decision/v1` | `PolicyDecision` + `Constraints` | Audit journal reason and decision fields |
| `data-envelope/v1` | `DataEnvelope` | In memory per model request |
| `tool-result/v1` | `ToolResult` | Returned to the model |
| `audit-event/v1` | `AuditEvent` | `journal.jsonl`, SHA-256 hash chain |
| `approval/v1` | `ApprovalRequest` | `approvals.json` |
| `sandbox-receipt/v1` (not in the proposal table) | `SandboxReceipt` | Embedded in `ToolResult`; only its ID is journaled |
| Budget ledger (not in the proposal table) | `BudgetSnapshot` | `budget-<task>.json` |

## 3. Not yet defined

- `sandbox-grant/v1` — no external, signed grant is issued. The runtime selects
  and reports a plan locally.
- `task-checkpoint/v1` — no durable model-run cursor, action-intent ledger or automatic recovery state. Native plan metadata checkpoints are available separately and do not resume a model session.
- Signed `action-request` / signed `tool-result` — requests and results are not
  signed, and the journal does not bind a canonical contract digest.
- Retention labels and lineage records — `DataEnvelope.lineage` is always empty.
- Policy administration, role and incident schemas.

## 4. Canonicalization warning

Signatures are computed over Rust's compact `serde_json` serialization with the
signature field removed (policy/identity) or over a SHA-256 digest of the
manifest without `integrity` (plugins). This is deterministic for this binary but
is **not** RFC 8785/JCS canonical JSON, and field order follows the Rust structs.
A third-party issuer must reproduce the same serialization exactly. There is no
signing CLI; the signing helpers are test-only. Freeze a canonicalization format
and ship issuer tooling before publishing an SDK.
