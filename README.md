# Jani 🦀

**Jani** is an independent **Rust-native, skill-aware AI coding agent**. Its agent runtime, tool dispatch, workspace handling and OpenAI-compatible model client are written in Rust.

Jani embeds no coordination engine. It works directly in a Git worktree, reads installed `SKILL.md` guidance on demand, and enforces its permissions in Rust. Collaboration workflows, when needed, are just skills or external processes that invoke Jani — they are not part of Jani's core or its privilege model.

**Status: source implementation, not yet production validated.** The project is deliberately strict by default: no edits or command execution unless explicitly enabled. The source archive is not a prebuilt binary. Before using it with a real repository, run the build and acceptance suite described below.

## What is implemented

- Rust binary with `--print`, `run`, `skills`, `doctor`, `--version` and `--help`.
- Native OpenAI-compatible Chat Completions client with JSON function/tool calling, configurable model, endpoint and bounded turn loop; no Python AI SDK.
- Discovers installed `SKILL.md` files and reads instructions on demand.
- Typed tools for non-sensitive working-tree listing and reads, explicitly scoped atomic file writes, and allowlisted test/git commands.
- Default-deny permissions with explicit relative write scopes.
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
2. **Rust tool boundaries:** only explicit relative paths inside `--write-path` scopes can be written; only allowlisted argv can run; protected and symlinked paths are refused.
3. **Process boundary:** children receive no shell, no provider API key, and have bounded time and output.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/TESTING.md](docs/TESTING.md).

## Security limitations

- Jani is not an OS sandbox and does not prevent arbitrary processes from modifying files outside its guarded interfaces. Use OS/container isolation for untrusted code. See [docs/SANDBOX_PROPOSAL.md](docs/SANDBOX_PROPOSAL.md) for a proposed optional OS-level sandbox.
- Even an allowlisted `cargo test`, `pytest`, `go test` or `npm test` can run malicious build scripts or test code. Use container sandboxing with restricted secrets and a disposable checkout.
- Path protections are vulnerable to a hostile process racing file replacement; use a sandbox or OS ACLs for isolation.
- This release uses single-run model context; recovery is reconstructible from repository state, not an automatically replayed Jani conversation transcript.
- `--max-steps` is a turn ceiling, not a monetary spending limit. Provider usage is not accounted for internally.
