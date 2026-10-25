# Jani 🦀

**Jani** is an independent **Rust-native, skill-aware AI coding agent**. Its agent runtime, tool dispatch, workspace handling and OpenAI-compatible model client are written in Rust.

Jani embeds no coordination engine. It works directly in a Git worktree, reads installed `SKILL.md` guidance on demand, and enforces its permissions in Rust. Collaboration workflows, when needed, are just skills or external processes that invoke Jani — they are not part of Jani's core or its privilege model.

**Status: source implementation, not yet production validated.** The project is deliberately strict by default: no edits or command execution unless explicitly enabled. The source archive is not a prebuilt binary. Before using it with a real repository, run the build and acceptance suite described below.

The [enterprise proposal pack](docs/jani_enterprise_proposals/README.md) is
partially implemented as a trusted local kernel. Its
[implementation review](docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md)
records what is enforced, what is missing, and why this build is not an
enterprise-ready release.

## What is implemented

- Rust binary with `run`, `--print`, `skills`, `doctor`, `sandbox`,
  `plugins`, `policy`, `audit`, `inspect`, `approvals`, `budget`, `identity`,
  `contract`, `--version` and `--help`.
- Native OpenAI-compatible Chat Completions client with JSON function/tool calling, configurable model, endpoint and bounded turn loop; no Python AI SDK.
- Discovers installed `SKILL.md` files and reads instructions on demand.
- Typed tools for non-sensitive working-tree listing and reads, explicitly scoped atomic file writes, and allowlisted test/git commands.
- Default-deny permissions with explicit relative write scopes. Operator flags are a
  ceiling; a signed work contract can narrow them but never widen them.
- Signed organization policy (Ed25519, tenant-bound, expiring) with default deny,
  explicit denies, risk tiers, approvals and budgets. Enterprise mode requires a
  signed policy and identity assertion.
- Signed identity assertions bound to tenant and principal.
- Data governance: classification, secret/PII detection and redaction, per-request
  data envelopes, and a fail-closed provider/region/purpose egress matrix. Empty
  allowlists deny; `"*"` is the only wildcard.
- A mediating tool broker that validates every request, enforces policy,
  capabilities, write scope and budgets, and journals the decision before acting.
- Hash-chained audit journal (`jani audit verify`, `jani inspect RUN_ID`) with
  fail-closed behavior when a record cannot be written.
- Persisted per-task budget ledgers for tool calls, wall time, tokens and priced
  USD spend, tightened across restarts.
- Single-use approvals with exact capability/resource/payload binding, TTL and
  separation of duties.
- Sandbox selection with no silent fallback: bubblewrap on Linux, a Windows Job
  Object on Windows, and explicit refusal when an isolation profile cannot be
  satisfied. Receipts report filesystem and network confinement honestly.
- Signed plugin manifest registry with capability cataloguing. Plugins are
  verified but **not executable** in this build.
- Protection against path traversal, symlink components and common credential paths. Protected paths are not a replacement for OS-level sandboxing.
- Children run with argv only (no shell), bounded time and output, and without the provider API key in their environment.
- `git commit` refuses pre-staged files outside the approved edit scope.

## Requirements

1. Rust and Cargo with a recent stable toolchain.
2. An AI provider that implements OpenAI-compatible `POST /chat/completions` tool calling; valid API key and model ID.
3. For write mode: explicit approved edit paths. For commands: an approved executable operation.

## Build

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
./target/release/jani --version
```

On Windows, use `target\\release\\jani.exe`. If Rust is not installed, see the official [Rust installation instructions](https://www.rust-lang.org/tools/install).

## Usage

```bash
export JANI_API_KEY=YOUR_PROVIDER_API_KEY
export JANI_MODEL=YOUR_TOOL_CAPABLE_MODEL
cargo run -- doctor
cargo run -- skills
cargo run -- --print "Summarize the main modules and their risks. Do not edit anything."
```

A read-only run needs no write permissions. To allow scoped work:

```bash
cargo run -- --print "Implement the requested change, then run the tests" \
  --allow-write --write-path src --write-path tests --allow-exec
```

`doctor` reports the worktree, discovered skills, model configuration, and active permissions. `JANI_API_BASE` defaults to `https://api.openai.com/v1`; adjust it for a compatible provider. A model call may incur usage charges.

The same options can be supplied through the environment: `JANI_WRITE_PATHS`, `JANI_EXEC_PROGRAMS`, `JANI_SKILL_DIRS`.

## Skills

A skill is regular Markdown with a YAML `name` and `description` frontmatter. Jani discovers `SKILL.md` files in the user's skill directory and in the repository (`.agents/skills`, `.claude/skills`, `skills`), then reads them on demand.

**A skill is advisory, not an executable security policy.** Rust tool boundaries enforce write scopes and the command allowlist regardless of what a skill says. If a workflow needs coordination with other agents, install the relevant collaboration skill or run an external coordinator that invokes Jani as a normal CLI process; Jani does not grant extra authority to either.

## Security model

1. **Operator permission:** writes and command execution are default-denied.
2. **Signed policy:** in enterprise mode a signed, tenant-bound policy must exist;
   it is default-deny and can only narrow operator flags. A work contract can only
   narrow the policy and operator scope.
3. **Rust tool boundaries:** only explicit relative paths inside `--write-path` scopes can be written; only allowlisted argv can run; protected and symlinked paths are refused. Every call passes through the broker, policy and audit journal.
4. **Data egress:** secrets are redacted and secret-classified content is never sent to a provider; provider, region, purpose and classification allowlists fail closed.
5. **Process boundary:** children receive no shell, no provider API key, and have bounded time and output. A verified sandbox backend is required for isolation profiles.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/TESTING.md](docs/TESTING.md), the [schema notes](docs/schemas/README.md) and the [implementation review](docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md).

## Security limitations

- **Platform confinement is incomplete.** bubblewrap provides filesystem and network confinement on Linux hosts that support it. The Windows Job Object bounds only the process tree and cannot satisfy enterprise isolation profiles. macOS has no backend. Do not treat this build as an OS sandbox on Windows or macOS.
- **Plugins cannot execute.** Manifests are verified and catalogued only; there is no WASM, sidecar or MCP runtime.
- **No identity or policy control plane.** There is no IdP/SCIM, role claim, policy
  administration API, key rotation/revocation, signed work contract, signed
  execution receipt or nonce replay cache. Approval roles are caller-asserted.
- **State is local and plaintext.** The journal, approvals and budget ledgers are
  plaintext JSON in `--state-dir`; there is no encryption, tenant partitioning,
  retention/erasure or external audit anchoring.
- **Signing interoperability is limited.** Signatures cover this binary's
  `serde_json` field order, not RFC 8785/JCS, and there is no issuer CLI. See
  [docs/schemas/README.md](docs/schemas/README.md).
- Even an allowlisted `cargo test`, `pytest`, `go test` or `npm test` can run malicious build scripts or test code. Use container sandboxing with restricted secrets and a disposable checkout.
- Path protections are vulnerable to a hostile process racing file replacement; use a sandbox or OS ACLs for isolation.
- This release uses single-run model context; recovery is reconstructible from repository state, not an automatically replayed Jani conversation transcript. There are no checkpoints or leases.
- `--max-steps` is a turn ceiling. Budgets bound tool calls, time, tokens and
  policy-priced USD, but provider billing is authoritative and is not reconciled
  after the run.
