---
id: JANI-OPS-004
title: "Command-line reference"
status: implemented-partial
owner: backend/server
version: 1.1.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/config.rs
tests: []
---

# Command-line reference

Use this page to find the command family or safety switch you need. It is an entry point, not a substitute for `jani --help` or the parser. When a flag's behavior or rejection rule matters, verify it against `backend/core/src/config.rs` and its tests.

## Command families

| Command | Purpose |
| --- | --- |
| `run` / `--print` | Execute a model-driven task subject to operator scope, policy and budgets |
| `doctor`, `skills`, `sandbox`, `plugins`, `policy`, `audit`, `inspect`, `approvals`, `budget`, `identity`, `contract` | Inspect environment, configuration and safety state |
| `agent`, `alerts`, `wake` | Operate local attention state; neither alerts nor wake automatically invokes a model |
| `mirror` | Produce deterministic critique and evidence-linked proposals from recorded runs |
| `mcp` | Access governed MCP sources under signed contracts and task delegation |
| `native TOOL OPERATION` | Run bounded indexing, planning, context, evidence, workspace and simulation operations |
| `--version`, `--help` | Print version or help without reading credentials or settings |

## Model and settings switches

- `--settings FILE` loads project settings from an explicit path. `JANI_SETTINGS` supplies the same path through the environment. Relative paths resolve against the worktree. An explicitly named missing file fails closed.
- Without `--settings` or `JANI_SETTINGS`, Jani reads `.jani/settings.json` if it exists. A missing default file is not an error.
- `--model-profile NAME` selects a named profile from the settings file. Without it, `defaultModel` applies. Selecting a profile when no settings file exists is an error.
- Precedence is **CLI flag > environment variable > settings file > built-in default**.
- Settings only select the model, API base, credential-variable name and step limit. They never grant write or command authority.

See [Configuration reference](CONFIGURATION_REFERENCE.md) for the schema and [the example](schemas/settings.example.json).

## Safety-relevant switches

- `--repo` selects the worktree; `--state-dir` selects persistent state.
- `--allow-write` requires at least one explicit relative `--write-path`. There is no implicit full-repository editing.
- `--allow-exec` permits only approved executable commands; `--exec-program` adds a constrained list of extra test runners.
- `--mode enterprise`, `--policy`, `--policy-key`, `--identity` and `--contract` supply enterprise authorization. Missing or invalid authority is refused.
- `--approve` in native planner verification is permitted only for a verified human identity.
- `--sandbox-mode` and network restrictions must not be used to weaken required confinement. `jani sandbox doctor` reports support, not certification.

## Operational rules

- Keep provider credentials in environment variables referenced by `apiKeyEnv`. Inline keys in settings are rejected.
- `--help` and `--version` must work without credentials or a readable settings file.
- No remote configuration control plane exists. Every default lives in the Rust parser.

## Acceptance

Test unknown flags, missing option arguments, invalid write scopes, rejected programs, protected credential paths, unavailable sandbox profiles, missing settings files and profile selection without settings.

## Maintenance

Review the exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
