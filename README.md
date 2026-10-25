# Jani

**Sentinel roadmap and compliance:** [monitoring and security contract](docs/SENTINEL_CONTRACT.md) · [release gates](docs/SENTINEL_RELEASE_GATES.md). The proprietary JSMT client/server reference implementation is deliberately not published in this public repository. No production monitoring functionality is implied.

**A Rust-native coding agent that works only inside authority you grant explicitly.**

Jani reads skills, calls an OpenAI-compatible model and edits a Git worktree. Every read, write and command passes through a Rust broker that checks operator scopes, signed policy, data-egress rules and budgets, then journals the decision before acting. Model output, skill text and tool responses are never treated as authority.

> **Status: supervised prototype, not production-validated.** The source builds and its local gates pass, but this is not an enterprise-certified release. Windows and macOS cannot satisfy enterprise isolation profiles, plugins do not execute, and no live-provider end-to-end session is recorded. Read [VALIDATION.md](VALIDATION.md) for the evidence and the remaining gaps before relying on it.

## Choose your path

| If you want to... | Start with |
| --- | --- |
| Install the binary | [Install](#install) |
| Try it safely in a few minutes | [Quick start](#quick-start) |
| Understand what stops a bad edit or command | [How authority works](#how-authority-works) |
| Review the security posture | [SECURITY.md](SECURITY.md) and [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) |
| See exactly what is tested and what is not | [VALIDATION.md](VALIDATION.md) and [docs/TESTING.md](docs/TESTING.md) |
| Assess enterprise or release readiness | [docs/PROJECT_CONTRACT.md](docs/PROJECT_CONTRACT.md), [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md), [implementation review](docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md) |
| Change the code | [CONTRIBUTING.md](CONTRIBUTING.md) |
| See what changed | [CHANGELOG.md](CHANGELOG.md) |
| Browse every subsystem document | [docs/INDEX.md](docs/INDEX.md) |

## What Jani is, and is not

Jani is a standalone coding agent. The agent loop, tool dispatch, workspace handling, policy checks and model client are Rust. It works directly in a Git worktree and reads installed `SKILL.md` guidance on demand; any collaboration workflow is an external process invoking Jani, not part of its privilege model.

Jani is **not** a hosted control plane, a multi-agent runtime, an OS sandbox on every platform, or an unattended operator. There is no identity provider integration, no distributed coordinator, no durable model checkpointing and no plugin executor. Those are stated limits, not hidden fallbacks: when a required control is unavailable, Jani refuses rather than silently downgrading.

## Install

From a clone, on Windows x64:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

The installer reads `releases/latest.json`, selects the artifact for your host, verifies its SHA-256 digest, refuses a mismatch, and copies the binary to `%LOCALAPPDATA%\Jani\bin`. It does not configure credentials or grant write or command authority. Add `-AddToPath` to update your user `PATH`. Layout, manual verification and the release process are in [releases/README.md](releases/README.md).

To build and install from source instead, on any supported platform:

```powershell
cargo install --path backend/server --locked
```

## Quick start

**Prerequisites**

- Rust 1.95.0, pinned by [rust-toolchain.toml](rust-toolchain.toml).
- A provider implementing OpenAI-compatible `POST /chat/completions` tool calling, plus an API key and a tool-capable model ID.
- Python 3.11 only if you want to run the synthetic corpus tooling.

If you installed the release binary above, skip the build and go to Configure.

**Build and verify**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release -p jani
./target/release/jani --version
```

On Windows use `target\release\jani.exe`. If Cargo is missing, follow the official [Rust installation instructions](https://www.rust-lang.org/tools/install).

**Configure and run read-only**

```bash
export JANI_API_KEY=YOUR_PROVIDER_API_KEY
export JANI_MODEL=YOUR_TOOL_CAPABLE_MODEL
./target/release/jani doctor
./target/release/jani --print "Map the main modules and their risks. Do not edit anything."
```

`doctor` prints the worktree, discovered skills, model configuration, settings source and active permissions. `JANI_API_BASE` defaults to `https://api.openai.com/v1`; point it at another compatible endpoint if needed. A model call may incur provider charges.

**Enable scoped work only when intended**

```bash
./target/release/jani --print "Implement the requested change, then run the tests" \
  --allow-write --write-path src --write-path tests --allow-exec
```

Writes require explicit relative scopes, and commands must match the allowlist. Use a disposable checkout or container for any run that executes project code.

## Settings

`.jani/settings.json` holds named model profiles and a step limit. A missing file is fine. Precedence is **CLI flag > environment variable > settings file > built-in default**.

```json
{
  "version": 1,
  "defaultModel": "fast",
  "models": {
    "fast": {
      "model": "gpt-5-mini",
      "apiBase": "https://api.openai.com/v1",
      "apiKeyEnv": "JANI_API_KEY"
    },
    "deep": {
      "model": "gpt-5",
      "apiBase": "https://api.openai.com/v1",
      "apiKeyEnv": "JANI_API_KEY"
    }
  },
  "maxSteps": 20
}
```

Select a profile with `--model-profile deep`. Use `--settings FILE` or `JANI_SETTINGS` for a different file; relative paths resolve against the worktree. `apiKeyEnv` names the environment variable holding that profile's key. An inline `apiKey` is rejected, unknown fields fail closed, and a profile's named variable is the only credential source it will use. Settings can change model selection and limits, never write or command authority. The full example is [docs/schemas/settings.example.json](docs/schemas/settings.example.json); field constraints are in [docs/schemas/README.md](docs/schemas/README.md).

Only `.jani/settings.json` is unignored; journals, approvals and other state stay out of Git.

## How authority works

Jani narrows authority at every layer. No layer can widen a layer above it, and none can be skipped by model text.

| Layer | What it decides | What it cannot do |
| --- | --- | --- |
| Operator flags | Whether writes or commands are allowed at all, and which relative paths or programs | Grant access outside its declared scopes |
| Work contract | Narrows the operator's allowed effects for one task | Widen operator or policy scope |
| Signed policy and identity | Tenant-bound allow/deny rules, risk tiers, approvals and budgets in enterprise mode | Be satisfied by an unsigned or wrong-tenant assertion |
| Mediating broker | Validates each call, enforces policy, scope and budget, journals before acting | Be bypassed by a tool, skill or model response |

Defaults are deny. Children run with argv only, no shell, bounded time and output, and no provider API key in their environment. Protected and symlinked paths are refused. Sandbox selection has no silent fallback: an unsatisfied isolation profile is an error, not a weaker mode.

## Capabilities at a glance

| Area | What exists | Boundaries |
| --- | --- | --- |
| Agent runtime | Bounded turn loop, OpenAI-compatible tool calling, on-demand `SKILL.md` discovery, typed read/write/command tools | Skills are advisory; Rust boundaries still enforce scope |
| Governance | Default-deny PDP/PEP, signed tenant-bound policy and identity, narrowing work contracts, single-use approvals with exact binding | No IdP, policy admin API, role claims or key rotation |
| Data protection | Classification, secret/PII detection and redaction, per-request envelopes, fail-closed provider/region/purpose egress matrix | No tenant-partitioned or encrypted local state |
| Audit and budgets | Hash-chained journal (`jani audit verify`, `jani inspect RUN_ID`), persisted tool/time/token/USD ledgers | Provider billing remains authoritative; spend is not reconciled |
| Sandboxing | bubblewrap filesystem/network confinement on supported Linux; Windows Job Object for the process tree | Windows/macOS cannot meet enterprise isolation profiles |
| Lifecycle and alerts | Local `agent`, `alerts` and `wake` commands with durable attention state | Alerts never launch tools; wake is not durable model resume |
| Extensions | Mirror retrospective critique, signed MCP delegation gateway, 12 governed native tools, verified plugin manifests | Plugins are not executable; MCP supports only authorized `tools/call` |

Detailed contracts: [ARCHITECTURE](docs/ARCHITECTURE.md), [AUTHORIZATION MATRIX](docs/AUTHORIZATION_MATRIX.md), [TOOL EXECUTION CONTRACT](docs/TOOL_EXECUTION_CONTRACT.md), [AGENT LIFECYCLE](docs/AGENT_LIFECYCLE.md), [ATTENTION PROTOCOL](docs/ATTENTION_PROTOCOL.md), [MCP GOVERNANCE](docs/MCP_GOVERNANCE.md), [MIRROR](docs/MIRROR.md), [NATIVE TOOLS](docs/NATIVE_TOOLS.md), [PLUGIN RUNTIME](docs/integrations/PLUGIN_RUNTIME.md).

### Mirror and MCP in one minute

**Mirror** deterministically reviews Jani's recorded runs, compares timelines and saves evidence-linked proposals only when explicitly asked. It never executes or self-applies a recommendation. The `jani-mirror` package has no model-client or coordination dependency.
<!-- jani-claim: {"id":"README-MIRROR-ENGINE","kind":"source_contains","path":"backend/mirror/src/lib.rs","needle":"pub fn run_report("} -->

**MCP governance** is a signed, fail-closed delegation gateway: the initiator's verified identity and task roles must match an approved server contract, remote results need signed authorization and classification receipts, and agent invocations of user-only commit tools are always blocked. Only the explicitly authorized `tools/call` transport is available.
<!-- jani-claim: {"id":"README-MCP-COMMIT-GUARD","kind":"source_contains","path":"backend/mcp/src/lib.rs","needle":"USER_COMMIT_REQUIRED"} -->

## Deliberate limits

- **Platform confinement is incomplete.** bubblewrap provides real filesystem and network confinement only on Linux hosts that support it. The Windows Job Object bounds a process tree but is not an OS sandbox. macOS has no backend. Do not describe Jani as sandboxed on Windows or macOS.
- **Plugins are verified, not executable.** Manifests are signed and catalogued; there is no WASM, sidecar or MCP plugin runtime.
- **No managed identity or policy control plane.** There is no IdP/SCIM, policy administration, key rotation/revocation, signed execution receipt or nonce replay cache; approval roles are caller-asserted.
- **Local state is plaintext.** Journal, approvals and budget ledgers are JSON in `--state-dir`, with no encryption, tenant partitioning, retention/erasure or external anchoring.
- **Signatures are not RFC 8785/JCS.** They cover this binary's `serde_json` field order, and no issuer CLI ships.
- **Allowlisted test commands are still code execution.** `cargo test`, `pytest`, `go test` or `npm test` can run malicious build scripts or test code. Use a container with restricted secrets and a disposable checkout.
- **Path checks can lose a race.** A hostile process replacing files concurrently needs OS ACLs or a sandbox, not path checks alone.
- **Context is single-run.** There is no automatic conversation checkpoint replay, and alert leases are not process-ownership leases.
- **Budgets are ceilings, not billing.** `--max-steps` and persisted budgets bound what Jani attempts; the provider invoice remains authoritative.

## Repository map

| Path | Purpose |
| --- | --- |
| `backend/core/` | Shared contracts, configuration, settings, identity, policy and governance primitives |
| `backend/server/` | The `jani` executable, agent loop, tools and CLI |
| `backend/store/` | Durable transactional state, including the attention store |
| `backend/mirror/` | Deterministic retrospective critique of recorded runs |
| `backend/mcp/` | Signed MCP delegation and response-receipt gateway |
| `backend/native/` | Twelve bounded native tools exposed through `jani native` and the broker |
| `contract/` | Protocol definitions |
| `tools/` | Architecture, documentation and claims gates, plus release packaging |
| `synthetic/` | Deterministic fault-corpus generation and readiness evaluation |
| `docs/` | Normative contracts, schemas and subsystem guides |
| `claims/` | Generated documentation-claims register and open findings |
| `releases/` | Versioned binaries, SHA-256 checksums and manifests; `latest.json` drives the installer |
| `install.ps1` | One-command, checksum-verifying installer for the current host |

The root intentionally has no `src/` or `crates/`; this is a virtual Cargo workspace. See [WORKSPACE ARCHITECTURE](docs/WORKSPACE_ARCHITECTURE.md).

## Verification helpers

- **Documentation claims:** `cargo run -p jani-claims -- sync` refreshes `claims/`; `-- check` fails if the register is stale, and `-- strict` also fails on open deterministic findings.
- **Documentation manifest:** `python tools/doccheck.py` verifies the manifest, frontmatter, IDs and source/test references.
- **Synthetic corpus:** `python -m synthetic.generate generate --output synthetic/generated/full_5m` then `verify` builds and checks a deterministic 5,000,000-case corpus. This proves corpus integrity, **not** that Jani executed those cases. Use [synthetic/readiness.py](synthetic/readiness.py) with independent execution receipts; see [synthetic/README.md](synthetic/README.md).
- **Architecture gate:** `cargo run -p jani-check`.

## Documentation and license

Start at [docs/INDEX.md](docs/INDEX.md). Source code and executable tests are the implementation authority; proposals and AI-authored summaries are not. Jani is licensed under the [MIT License](LICENSE).
