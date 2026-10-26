---
id: JANI-OPS-004
title: "Command-line reference"
status: implemented-partial
owner: backend/server
version: 1.3.0
last-reviewed: 2026-10-26
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
- A failed turn restores **model conversation context** to the checkpoint before that prompt; a failed turn and partial model tool results do not enter the next prompt context. This does **not** undo file changes, commands or external side effects already performed. The interface directs the operator to inspect the journal and worktree before retrying. History itself is process-local and is not persisted across restarts.
- With redirected or piped stdin, `jani repl` warns that stdin is not a terminal and reads commands until EOF. Answers go to stdout; the banner and `jani> ` prompt go to stderr.

On the initial empty full-screen view, `F1`–`F4` prefill read-only starter task drafts (repository map, change plan, security review, test coverage). Shortcuts never submit a prompt or execute a tool; review and press Enter to send. The terminal displays a configured sandbox **profile**, not proof of operating-system isolation. Use `jani sandbox doctor` for the actual backend capabilities.

When **both** stdin and stdout are terminals, the session runs as a full-screen terminal interface: an alternate-screen workspace with a scrollable conversation transcript and its scrollbar, a multi-line input box, a frozen-authority meta line, key hints, a rotating tip and a status bar. Rendering requires a terminal of at least 40x10; smaller windows show a resize message instead of a clipped layout. A terminal emulator is not required for correctness: if either stream is redirected, Jani falls back to line mode with a warning, so pipes and existing automation keep working.

Keys in the full-screen interface:

| Key | Effect |
| --- | --- |
| `Enter` | Submit the current input; with the command palette open, run the highlighted command |
| `F1`–`F4` | On the empty welcome screen, prepare (without submitting) a safe repository inspection, plan, risk review or test-coverage draft |
| `/` | On an empty input, open the command palette |
| `Tab` | Complete a slash command; with the palette open, fill the highlighted command name or model profile into the input |
| `Up` / `Down` | Walk input history; with the palette open, move the selection and wrap at the ends |
| `Left` / `Right`, `Home` / `End` | Move the cursor; `Ctrl+A` / `Ctrl+E` jump to the ends. With an empty input, or with `Ctrl` held, `Home` jumps the transcript to its top and `End` resumes following |
| `Backspace` / `Delete` | Edit by Unicode scalar, not byte |
| `PgUp` / `PgDn` | Scroll the transcript ten lines; scrolling up pauses follow mode until you return to the bottom |
| Mouse wheel | Scroll the transcript three lines per notch; `Home` / `End` and `PgDn` also resume following |
| Mouse click | On a `▸ think` label, expand or fold that one block; on the scrollbar, jump to that proportion of the transcript |
| `Ctrl+T` | Expand or fold the most recent thinking block; blocks start folded |
| `Esc` | Clear the input; with the palette open, close it and clear the input |
| `Ctrl+U` | Clear the input before the cursor |
| `Ctrl+C` | Clear a non-empty input, or exit on an empty input; during a turn it exits after that turn |
| `Ctrl+D` | Exit when the input is empty |

Typing `/` into an empty input opens a command palette above the input box. It lists every slash command with its usage and a one-line summary; typing filters by command name, `Up`/`Down` move through the matches, `Enter` runs the highlighted command through the same checked path as typed input, `Tab` fills the name into the input for review, and `Esc` closes the palette and clears the input. `/approval` requires an ID, so selecting it fills `/approval ` and leaves the cursor in the input for the ID; every other command runs immediately (`/inspect` uses the latest turn). The palette reads the same command catalog as `/help` and is only a launcher: it never widens scopes, never decides an approval and never bypasses the frozen authority checks.

`/model` opens a second-level picker instead of running immediately. It lists the model profiles from the active settings file (the current one is marked `current`) and, when a query matches no profile, offers the typed text as a raw model ID that keeps the current transport and credential. Typing filters by profile name or model ID, `Up`/`Down` move, `Enter` switches the running session, `Tab` completes the profile name, and `Esc` closes the picker. A profile switch applies that profile's model, API base, protocol, output ceiling and credential binding; a raw model ID keeps the current API base, protocol and credential and clears the profile. Switches are runtime state, not authority: policy, identity, contract, write scopes, executable programs, sandbox and network remain frozen, and every switch is journaled as an `agent.model` event before it takes effect. A model with no resolvable credential, or a non-HTTPS non-localhost API base, is refused and the current selection stays active. The transcript continues across the switch.

Once the transcript is taller than the viewport, Jani reserves its rightmost column for a vertical scrollbar and the tip line reports how many lines sit below the bottom; windows at the 40-column minimum keep the full width for text and scroll without a bar. Follow mode pins the view to the newest line; any scroll-up gesture unpins it and the view stays where you put it while the next turn streams. Mouse capture is on, so the pointer scrolls and clicks instead of selecting text in the window — use the terminal's own selection override (Shift in Windows Terminal and most emulators) to copy.

The interface never decides an approval and never prints provider credentials. The per-turn run banner that line mode writes to stderr is suppressed in full-screen mode so it cannot corrupt the frame; the same run identity is available through `/status` and `/inspect`. Assistant answers are rendered as Markdown: headings, bold and italic, inline code, bullet and numbered lists, block quotes, rules, tables and fenced code blocks are formatted, and the raw markers are not shown. Rendering is style-aware for wrapping and falls back to literal text for malformed constructs.

While a turn runs, the interface streams progress. The meta line shows the current step and action, each finished tool adds a `tool` entry with a short summary (for example `reading notes.txt`), and the answer preview is rendered as Markdown as it arrives. Model reasoning, when the provider emits it, becomes its own thinking block with a one-line `▸ think` summary: click that line, or press `Ctrl+T` for the newest block, to expand it, and a block you left open stays open once the turn is committed to the transcript. The view is pinned to the newest line until you scroll up with `PgUp`, the mouse wheel or `Home`; the tip line then counts the lines below the bottom, and `PgDn`, `End` or a wheel-down gesture resumes following.

Streaming keeps the same output inspection as non-streaming turns: preview text is inspected before it is shown, only complete lines are released, and an unterminated private-key block is withheld from its header. As a consequence, an answer with no newlines appears only when the turn completes, and a streaming request cannot be retried after the provider has sent response headers. Providers must support SSE streaming; line mode uses the non-streaming request path.

Slash commands are read-only with respect to authority:

| Command | Effect |
| --- | --- |
| `/help` | List commands |
| `/status` | Show frozen authority, task, model, run count and history size |
| `/model [NAME]` | List model profiles, or switch the session to a profile or raw model ID |
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
