---
id: JANI-OPS-004
title: "Command-line reference"
status: implemented-partial
owner: backend/server
version: 1.2.0
last-reviewed: 2026-10-25
source:
  - backend/core/src/config.rs
  - backend/server/src/repl.rs
  - backend/server/src/tui.rs
  - backend/server/src/agent.rs
tests:
  - backend/server/tests/cli.rs
  - backend/server/tests/e2e.rs
---

# Command-line reference

Use this page to find the command family or safety switch you need. It is an entry point, not a substitute for `jani --help` or the parser. When a flag's behavior or rejection rule matters, verify it against `backend/core/src/config.rs` and its tests.

## Command families

| Command | Purpose |
| --- | --- |
| `run` / `--print` | Execute a model-driven task subject to operator scope, policy and budgets |
| `repl`, bare `jani` | Open an interactive session that keeps conversation across turns under one task; bare `jani` does this only when stdin is a terminal, otherwise it prints help |
| `doctor`, `skills`, `sandbox`, `plugins`, `policy`, `audit`, `inspect`, `approvals`, `budget`, `identity`, `contract` | Inspect environment, configuration and safety state |
| `agent`, `alerts`, `wake` | Operate local attention state; neither alerts nor wake automatically invokes a model |
| `mirror` | Produce deterministic critique and evidence-linked proposals from recorded runs |
| `mcp` | Access governed MCP sources under signed contracts and task delegation |
| `native TOOL OPERATION` | Run bounded indexing, planning, context, evidence, workspace and simulation operations |
| `--version`, `--help` | Print version or help without reading credentials or settings |

## Model and settings switches

- `--settings FILE` loads settings from an explicit path. `JANI_SETTINGS` supplies the same path through the environment. Relative paths resolve against the worktree. An explicitly named missing file fails closed.
- Without `--settings` or `JANI_SETTINGS`, Jani reads `<worktree>/.jani/settings.json`, then the user-wide `~/.jani/settings.json` (`%USERPROFILE%\.jani\settings.json` on Windows). `JANI_USER_SETTINGS` overrides the user-wide path. Only the first file found is used; files are not merged. A missing default file is not an error.
- `--model-profile NAME` selects a named profile from the settings file. Without it, a top-level `model` naming a profile applies, then `defaultModel`. Selecting a profile when no settings file exists is an error.
- `--model ID` overrides the selected profile's model. `--protocol openai|anthropic` and `--max-output-tokens N` override profile transport settings; `JANI_PROTOCOL` and `JANI_MAX_OUTPUT_TOKENS` are the environment equivalents.
- Precedence is **CLI flag > environment variable > settings file > built-in default**.
- Settings select the model, API base, credential source and step limit, and supply narrowing permission rules. They never grant write or command authority.

See [Configuration reference](CONFIGURATION_REFERENCE.md) for the schema and [the example](schemas/settings.example.json).

## Interactive terminal

`jani` with no arguments opens the interactive session when stdin is a terminal; with redirected or piped stdin it prints help instead. `jani repl` opens the same session explicitly regardless of stdin type. Both run over the governed agent loop used by `run`, keep conversation history across turns and fix authority at startup:

- One task ID spans the session; every prompt typed in the session gets its own run ID and journal entry. A prompt argument or `--run` on the command line is rejected because those belong to `run`.
- Policy, identity, contract, worktree, write scopes, executable programs, sandbox profile and network policy are loaded once. Slash commands cannot widen them; restart Jani to change authority.
- History is retained and trimmed at user-turn boundaries when it exceeds 256 KiB, always keeping the system prompt and the newest turn. Tool output is clipped at 64,000 bytes.
- A failed turn rolls the conversation back to the checkpoint taken before that prompt, so a partial tool call cannot poison later turns.
- With redirected or piped stdin, `jani repl` warns that stdin is not a terminal and reads commands until EOF. Answers go to stdout; the banner and `jani> ` prompt go to stderr.

When **both** stdin and stdout are terminals, the session runs as a full-screen terminal interface: an alternate-screen workspace with the conversation transcript, a multi-line input box, a frozen-authority meta line, key hints, a rotating tip and a status bar. Rendering requires a terminal of at least 40x10; smaller windows show a resize message instead of a clipped layout. A terminal emulator is not required for correctness: if either stream is redirected, Jani falls back to line mode with a warning, so pipes and existing automation keep working.

Keys in the full-screen interface:

| Key | Effect |
| --- | --- |
| `Enter` | Submit the current input |
| `Tab` | Complete a slash command |
| `Up` / `Down` | Walk input history; the in-progress draft is restored when you return |
| `Left` / `Right`, `Home` / `End` | Move the cursor; `Ctrl+A` / `Ctrl+E` jump to the ends |
| `Backspace` / `Delete` | Edit by Unicode scalar, not byte |
| `PgUp` / `PgDn` | Scroll the transcript |
| `Esc` | Clear the input |
| `Ctrl+U` | Clear the input before the cursor |
| `Ctrl+C` | Clear a non-empty input, or exit on an empty input; during a turn it exits after that turn |
| `Ctrl+D` | Exit when the input is empty |

The interface never decides an approval and never prints provider credentials. The per-turn run banner that line mode writes to stderr is suppressed in full-screen mode so it cannot corrupt the frame; the same run identity is available through `/status` and `/inspect`. Text that the model streams is not rendered incrementally; each turn appears when it completes, with a spinner and hint while the turn is running.

Slash commands are read-only with respect to authority:

| Command | Effect |
| --- | --- |
| `/help` | List commands |
| `/status` | Show frozen authority, task, model, run count and history size |
| `/skills` | List discovered skills |
| `/inspect [RUN_ID]` | Show journal events for a run, defaulting to the latest turn |
| `/budget` | Show the persisted task budget |
| `/approvals` | List approval requests |
| `/approval ID` | Attach an approval a verified approver already decided |
| `/new` | Clear conversation history; the task and budget continue |
| `/exit`, `/quit` | End the session |

Approvals stay external. A denied or approval-gated tool call reports the request ID, but the session never decides it. After a separate approver acts, `/approval ID` reloads the approval store and attaches it only if the request belongs to this tenant and task, is approved, has not expired, and was decided by someone other than the session principal. Unknown, pending, expired or foreign requests are refused.

## Safety-relevant switches

- `--repo` selects the worktree; `--state-dir` selects persistent state.
- `--allow-write` requires at least one explicit relative `--write-path`. There is no implicit full-repository editing.
- `--allow-exec` permits only approved executable commands; `--exec-program` adds a constrained list of extra test runners.
- `--mode enterprise`, `--policy`, `--policy-key`, `--identity` and `--contract` supply enterprise authorization. Missing or invalid authority is refused.
- `--approve` in native planner verification is permitted only for a verified human identity.
- `--sandbox-mode` and network restrictions must not be used to weaken required confinement. `jani sandbox doctor` reports support, not certification.

## Operational rules

- Keep provider credentials in environment variables referenced by `apiKeyEnv`. An inline `apiKey` is accepted when explicitly configured, but the file then holds a secret: `doctor` warns about it, never prints the value, and the file should stay out of version control.
- `--help` and `--version` must work without credentials or a readable settings file.
- No remote configuration control plane exists. Every default lives in the Rust parser.

## Acceptance

Test unknown flags, missing option arguments, invalid write scopes, rejected programs, protected credential paths, unavailable sandbox profiles, missing settings files and profile selection without settings.

## Maintenance

Review the exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
