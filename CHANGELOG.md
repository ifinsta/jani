---
id: JANI-GOV-002
title: "Changelog"
status: implemented-partial
owner: project
version: 1.4.0
last-reviewed: 2026-11-25
source:
  - README.md
  - backend/server/src/update.rs
  - tools/release_signing.py
  - install.ps1
  - tools/release.ps1
tests:
  - backend/server/tests/update_e2e.rs
  - tests/test_release_catalog.py
---

# Changelog

This page records what changed and what it means for compatibility. It is a development record, not a release certification. Evidence for the current state is in [VALIDATION.md](VALIDATION.md).

## Release status

- **No Git tags exist.** The workspace version in `Cargo.toml` is `0.1.2`.
- `releases/` is a commit-based channel, not a tagged release feed. `releases/agent/windows/v0.1.2/` contains the Windows binary built from commit `c2b8b60` with clean build inputs (SHA-256 `5c11ec31…`, 9,818,112 bytes); the installed copy at `%LOCALAPPDATA%\Jani\bin\jani.exe` has the same digest.
- This page describes commit `c2b8b60` (2026-10-27). Later documentation, probe and evidence changes are listed under Unreleased.

## Unreleased: the web workspace for people who are not engineers

- `jani ui` serves a redesigned workspace (`backend/server/src/ui/index.html`), the same data and the same trust boundary (loopback or socket, token, nonce CSP, no third-party hosts) in plain language. Today shows what needs you first, then tasks to pick up and recent work. Work is a run, not a transcript: state, activity, and cards that stop before anything changes (a command consent, a question, an approval) and say exactly what would happen. Tasks resume through `/api/session/resume`. How it fits follows one run from what it read, through the rules and the people who decide, to what changed and the record. Proof is the journal in sentences with a re-verify button, evidence downloads and unfinished work; Rules, Insights and Setup replace Policy, Mirror/Agents/Lab and System/Native/Updates. A "Show technical details" switch keeps digests, reason codes and actions one click away.
- Everything shown is read from the running process; nothing is sample data, and empty pages say nothing has happened yet. The earlier prototype is in `design/missions/`.
- The workspace is organisation- and role-aware: the rail shows the organisation (`JANI_CLOUD_TENANT`) and your role (Member, Approver, or Operator on a local machine). Approvers get **Approvals** (an inbox that sorts by risk and expiry, searches and filters, with two-click decisions and no bulk approve) and **Organisation** (what is in force, and the existing emergency stop). Hosted members do not see operator pages. Organisation also lists, as "Not built", what a multi-organisation rollout still needs: directory sign-in and provisioning, a mission catalogue, per-organisation policy, usage and cost, fleet health, central audit and data residency. The in-app inbox keeps the `data-testid` hooks of `/cloud/approver`, which still works.
- The consent shortcuts (`y`, `s`, `a`, `n`) and question digits still work after focusing inside the conversation. `cloud/e2e/cloud.test.mjs` selectors were updated for the new pages and have **not** been run here (they need Docker and an accounts server).

## Unreleased: delivery

- **`jani deliver "TASK" --accept CMD`** runs a task on its own git branch (a worktree; your checkout is untouched), then has the **host** run the acceptance checks, which the model cannot run, skip or edit. A failing check goes back to the agent for up to `--repair-rounds` turns of the same conversation. Changes inside the write scope are committed (hooks disabled) with the outcome in the message; `--push` and `--pr` publish if asked. The report (`deliveries/<id>/report.md`) lists every check, the diff, and any test files the agent changed. Outcomes are `verified`, `unverified` (no checks declared) or `failed`; a false "all tests pass" is `failed`. See [DELIVERY.md](docs/DELIVERY.md).
- Journal: `acceptance.run` and `delivery.run` events.
- **Ask before guessing** ([ASK.md](docs/ASK.md)): an `ask` tool (question plus up to six options). In the web workspace it is a card with option buttons and a text box, in the terminal UI a panel (digit or typed words), in `--print` and the line REPL a prompt on the terminal; with nobody there the turn ends on the question, and in the work queue the task becomes `blocked`, keeps its worktree and conversation, runs no checks and commits nothing, and resumes with `jani work answer ID TEXT`. Evals score an unanswered question as `asked_instead_of_finishing`. Under a work contract or signed policy, `agent.ask` and `memory.write` must be permitted like any action.
- **Durable work** ([WORK.md](docs/WORK.md)): `jani work add|list|run|show|answer|cancel|retry` is a queue of tasks with leases. A worker that dies mid-task is replaced: the lease lapses, the next worker reuses the worktree and **resumes the saved conversation**, and a worker presumed dead cannot overwrite the new one's result (fencing; tested with SIGKILL). A task carries no authority; the worker's flags do. Conversations resume through the task store's epoch-fenced checkpoints (`jani resume`), which are now **redacted** and restarted under the fresh system prompt. A `remember` tool and `jani memory list|add|forget` keep short project notes for the next session, scrubbed, bounded, journaled and labelled advisory.
- **`jani eval run|validate|compare`** measures completion on a suite (`eval/starter`: eight small tasks). Visible checks drive the repair loop; **hidden checks** the agent never sees decide `solved`, so special-casing the visible test scores zero. `validate` proves each case is fair without any model. Reports pass@1, pass@n, clean solves (tests untouched), tokens, rounds and failure causes. Verified with scripted models only; no real-model result is recorded yet. See [EVAL.md](docs/EVAL.md).

## Unreleased: smart command execution

- **`--exec smart`**: the agent is no longer limited to a fixed list of test commands. Read-only inspection (`ls`, `grep`, `rg`, `find`, `git status`...) runs on its own; builds and tests (`cargo`, `npm run`, `make`, `pytest`...) run on their own when a sandbox confines them (worktree read-only apart from the write scope, no network) and ask otherwise; privilege escalation, disk tools, deleting outside the worktree, force-push and publishing are always refused; everything else asks the operator. See [EXECUTION.md](docs/EXECUTION.md).
- **The operator answers in the web workspace**: Allow once, for the session, always (saved as an operator rule), or deny, with keyboard shortcuts. Cancelling a turn answers no. Under a signed policy, an unclassified command needs a signed approval instead.
- **`jani exec rules|allow|deny|forget|check`** manages operator rules in `<state-dir>/exec-rules.json` and explains any verdict. The model and the settings files cannot write rules; an unreadable file stops all commands.
- **`--print` and the line-based REPL ask too** when a person is at an interactive terminal: the question goes to stderr, the answer is read from the terminal itself (never from piped input), Enter denies. With no terminal the command is still refused with instructions.
- **Windows**: the line prompt reads from the console input buffer (`CONIN$`), falling back to stdin on terminals that are not consoles (mintty); it understands `\r\n` and a stray Ctrl+Z; the line-mode REPL no longer holds the stdin lock across a turn. The current pinned MSVC build is clippy-clean and the real Windows console probe passes, including terminal-origin consent prompts.
- **The terminal UI asks too**: a panel above the composer (`Alt+Y` once, `Alt+S` session, `Alt+A` always, `n`/Esc deny; Ctrl+C cancels and denies), with each answer noted in the transcript. Approval requires a fresh key press, exact Alt modifiers, focus inside the card and a fully rendered question; clipped layouts disable approval shortcuts.
- Journal events record why each command ran or was refused (`exec_confined_build`, `exec_operator_once`, `exec_consent_required`, ...).
- A confining sandbox now opens a tool's build-output directory (`target/`, an existing `build/`, ...) for writing; nothing else in the worktree.
- Fixed: the web workspace hid the first message of every session (and kept the welcome screen up during the first turn).

## Unreleased: fail-closed security and correctness hardening

- **Settings cannot silently reroute credentials.** Auto-discovered worktree settings containing top-level or profile `apiBase`/`apiKeyEnv` are refused before provider connections or state creation. Explicit `--settings`/`JANI_SETTINGS` and the trusted user-settings path opt in deliberately; permission and step limits still narrow runtime authority.
- **URL and command parsing is narrower.** Malformed authorities, userinfo, backslashes, controls and invalid ports are rejected; attached or clustered execution override flags no longer qualify as safe inspection. Permission glob matching is bounded, and Windows case, alternate-data-stream and trailing-dot aliases cannot evade protected-path checks.
- **DLP fails closed on incomplete secrets.** Quoted JSON/YAML assignments, escaped and truncated values, short unquoted values and unmatched private-key headers are withheld. Streamed private-key bodies never reach the browser, and an unterminated block is rejected without poisoning the next turn.
- **Native and skill reads obey current Read policy.** Direct source reads check policy at access time; existing index, context, evidence and workspace metadata are filtered by current rules. Repository skill roots and leaves reject symlink/reparse-point escapes, and frontmatter names cannot inject prompt text.
- **Approvals are tenant- and human-bound.** Every mutation reloads disk state under a stable cross-process lock and persists atomically, preventing stale stores from resurrecting consumed requests. Wrong-tenant, non-human, wrong-role and self-approval decisions are refused.
- **MCP delegation is bound to its server and request class.** Signed server ID/audience, exact resource scope, role/entitlement and request classification are verified; secret requests are refused and the classification is part of the authorized digest.
- **Windows children join their Job Object before execution.** Processes start suspended, are assigned to the Job Object, and resume only through the enumerated primary thread. Canonical `\\?\` worktree paths are retained for containment checks but normalized at the child working-directory boundary.
- **Model transport is bounded and cancellable.** Aggregate bodies, SSE lines/events and retry backoff have explicit limits; cancellation is checked before requests, during reads, between retries and before accepting replies. Cancellation is cooperative and does not preempt a blocked synchronous transport call.
- **Updates require explicit release trust.** Repeatable `--update-key`/`JANI_UPDATE_KEYS` roots verify a domain-separated Ed25519 manifest signature before artifact download. Existing `--to` targets are never executed for inspection; unknown targets require `--force`, and only a downloaded, hash-verified candidate runs with built-in and configured provider credentials removed.
- **Release tooling signs before publishing.** Windows and Unix typed installers require an explicit trusted public key; the publisher requires an out-of-tree Ed25519 private key and signs before advancing the typed channel. Frozen legacy v1 installation remains a separate explicit opt-in. Windows binaries are still not Authenticode-signed.
- **UI secrets and consent are stronger.** Browser tokens and CSP nonces use OS entropy, consent cards cannot steal composer focus, and TUI approval shortcuts cannot fire from key repeats, modifier combinations, hidden questions or another pane.
- **Journal appends and evidence views avoid repeated full scans.** Append handles cache the chain head and reread only the tail after another writer changes the file; UI journal, run and evidence views use one verified snapshot per request. Same-length external tampering still fails evidence verification, and large tail records remain supported.
- **Windows verification covers 438 Rust tests with zero failures.** The serial workspace run includes 187 server unit tests, 70 server integration tests, 126 core tests and 55 tests in the remaining crates. A real Windows console probe passes streaming, rendered-layout, plain-key rejection and deliberate `Alt+Y` consent checks; a connected-browser trial confirms composer focus isolation, explicit approval, real command execution and a verified four-event audit chain.
- **Release gates stay current.** CI installs the pinned toolchain components explicitly and runs the strict claims register gate; the Windows verification script runs clippy, tests, architecture, claims, documentation and a locked release build serially to avoid MSVC linker memory exhaustion.



## Unreleased: turns end in a deliverable, not in a cutoff

- **A spent step allowance is a handoff, not a failure.** Before, a turn that used all its steps failed with "model stopped after 20 turns without a final answer" and the conversation was rolled back to the last successful turn, discarding everything the agent had read. Now Jani asks the model, with tools forbidden, for a handoff (done and verified, not done, next steps), records the turn as completed (`step_checkpoint`), keeps the conversation, and "continue" resumes.
- **The default allowance is 100 steps** (was 20) and the settings ceiling is 400 (was 80). It is a backstop; real limits remain the token and tool-call budgets, cancellation, and the approval gates.
- **Runway warning:** four turns before the end the model is told to stop exploring, finish and verify.
- **Repeat detection:** the same call with the same arguments three times in a turn gets a note in its result; six ends the turn with a handoff (`repeated_calls`).
- The system prompt now says to work toward a deliverable, not a number of calls.
- Mirror still reports `step_budget_exhausted` for checkpointed runs.

## Short tool names and rendered hash-led tables — `c2b8b60`

- The model-facing workspace tools are now `list`, `read`, `write` and `command` (previously `workspace_list`, `workspace_read`, `workspace_write` and `workspace_command`); `skill_read` and `native_tool` keep their names. The transcript shows the short names. Journals are unaffected because they record actions such as `repo.read`, and project permission rules still map through `Read`, `Write` and `Bash`; the broker rejects the old names as unknown.
- Markdown tables whose first column header is `#` now render as tables. The heading parser previously claimed the line before the table parser saw it, so a table with a row-number column was emitted as raw pipe text. The real-console probe now streams a table with a `#` header and asserts the rendered header and row.
- `v0.1.2` release proof recorded in `validation/updates/2026-10-27/`: the typed artifact passed all 52 boolean console-probe checks (rendered `#` table, folded section with the last three `read` calls), `cargo test --workspace --all-targets` passed 260 tests, and `jani update install` over a loopback origin replaced the installed 0.1.1 bytes with 0.1.2 (SHA-256 `5c11ec31…`) under the channel manifest. The command ran from the installed 0.1.1 image, so it reported `restartRequired`; a fresh invocation reports `jani 0.1.2`. The live 0.1.0 session (PID 9836) was not killed.

## Unreleased

- Fix a Windows-only journal regression introduced with the interleaved-writer fix: the writer re-read the chain head through a second file handle while holding an exclusive lock, which Windows rejects with `ERROR_LOCK_VIOLATION` (os error 33), so every append failed. Appends now read the tail through the locked handle, and inspection takes a shared lock so it waits for a writer instead of failing. A journal already broken by the earlier stale-head bug remains unverified; rotate it with an operator-kept backup rather than rewriting its chain.

- The workspace UI now follows a code-editor layout: title bar with menus, command box and layout toggles; an activity bar of icons; a collapsible sidebar; editor tabs for the graph, terminal and each screen; an optional inspector and panel; a status bar; and a command palette (Ctrl+Shift+P). Panes are optional, draggable and remembered. See `docs/WEB_WORKSPACE.md`.

- jani-cloud now isolates each workspace's network (own namespace, outbound only through an allowlist proxy to the model provider), enforces per-person memory, process, CPU, file and disk limits, listens on per-user Unix sockets instead of loopback ports, and adds designated approvers: an operator-listed person can approve another person's held action through an approver page, using Jani's own approval store (separation of duties, role, expiry) with a hash-chained decision journal. cgroup v2, the Dockerfile and bubblewrap inside a container remain untested. See `cloud/README.md`.
- `cloud/` adds jani-cloud, a prototype single-container Linux deployment: people sign in with ifinsta (`siwi` against `accounts`), and each gets a private worktree, journal and `jani ui` process, optionally under their own Unix user and bubblewrap, reached through an authenticated reverse proxy that keeps the workspace token server-side. `jani ui` gains a `proxied` flag in `/api/overview` and, when proxied, shows the signed-in account with Sign out and Sign out everywhere. 13 end-to-end tests run against a real `accounts-server`. The Dockerfile has not been built, and bubblewrap inside a container, TLS, Redis and a live model are unverified. See `cloud/README.md`.

- `jani ui` serves a local browser workspace (see [Web workspace](docs/WEB_WORKSPACE.md)): a run graph of broker decisions, an inspector, approvals, the verified audit journal, budgets and an interactive session that reuses the terminal's slash commands. It binds loopback only, requires a per-process token and the exact loopback `Host`, serves a nonce-based CSP and never widens the authority the process was started with. Approval decisions need a verified identity and keep the store's separation-of-duties rules. It also has full-page Recovery, Policy, Evidence, Mirror, Agents, Lab, System, Native tools and Updates screens that call the same subsystems as their commands and refuse anything that mutates state. Covered by `backend/server/tests/ui.rs`; a connected-browser trial against a scripted loopback provider confirms pending-command focus isolation, explicit approval, real tool execution and an intact audit chain.

- `v0.1.1` release proof recorded in `validation/updates/2026-10-27/`: the typed artifact passed all 51 boolean console-probe checks (folded tool section with `last 3 shown`, expansion to five calls, scrollbar, palette, model picker). Real-channel evidence shows two 0.1.0 device copies upgraded to 0.1.1 with digest verification and a receipt, a repeated install that was up-to-date with no download, a watch process that started on v0.1.0 and installed v0.1.1 published while it ran over eight cycles, and update delivery over a loopback origin that replaced the installed `%LOCALAPPDATA%` client from 0.1.0 to 0.1.1 with the channel SHA-256. The live 0.1.0 session was not killed; it keeps its mapped image until restarted, and the installed bytes are already 0.1.1. The probe now derives its status-line marker from the binary's `--version` and waits for the full model-picker query, removing two hardcoded-version and settle-race failures.

- Self-distributed update delivery: `jani update check|install|watch` reads the typed release channel over HTTPS (loopback HTTP is allowed for local trials), verifies a domain-separated Ed25519 manifest signature against explicit operator trust roots, then verifies the artifact's exact size and SHA-256 before executing the downloaded candidate's `--version` with provider credentials removed. It replaces one or more local binaries (default: the running executable) with rename-aside on Windows and records a receipt under `<state-dir>/updates/`. Redirects, foreign platforms/targets, dirty builds, unsafe names, untrusted signatures and digest or size mismatches are refused before replacement. Existing targets are fingerprinted without execution and unknown versions require `--force`. `watch` polls the channel and applies releases published while it runs; an unreachable origin produces an error cycle instead of an exit, and the next reachable poll installs the release. New `tools/serve_updates.py` serves `releases/` for loopback trials, and an end-to-end suite covers multi-target push, a release published mid-watch, an origin outage and recovery, replacement of the running executable with `restartRequired`, target non-execution, signed-manifest trust failures and tamper rejection. The initial unsigned delivery described in earlier validation records is superseded by this explicit-trust workflow.

- Tool calls in the full-screen transcript now group into a foldable section per assistant turn: the header counts calls and failures, the three most recent calls stay visible with durations when collapsed, and clicking the header or Ctrl+T expands the whole turn and toggles it back. The real-console probe drives five calls through this path.

- Model transport hardening: an SSE stream that ends without a completion marker is rejected as truncated; connect timeout and bounded retry with exponential backoff cover transport errors and selected 408/429/5xx responses; the 180-second request timeout remains.

- Autonomous assignment proof: an end-to-end test drives `workspace_list` → `workspace_write` → `workspace_read` → final answer through the real broker, policy, budget and journal, asserting the written deliverable, journaled actions and a verified audit chain.

- Reliability soak: `tools/soak.py` ran 2,000 turns against a fault-injecting provider (1,117 injected HTTP 503/429, dropped and truncated faults at a 35% failure rate, maximum consecutive streak 4) with 2,000 completed runs, 0 failed runs, 80 tool reads and `audit verify` ok. The accelerated run simulates 66.67 hours (2.78 days) at a 120-second turn interval and is recorded in `validation/soak/2026-10-26-accelerated.json`; this is simulated time, not wall-clock days.

- `/skills` in the full-screen interface now renders a structured catalog instead of one dim paragraph: an accent header with the discovered count, each skill name marked and bold, its description in normal text, and a faint `SKILL.md` path with the Windows verbatim prefix and home directory shortened. Long descriptions and paths wrap to the transcript width, empty descriptions are labelled, and skills are separated by blank lines. The command remains read-only and makes no provider call. Line-mode `/skills` and `jani skills` keep their existing plain one-line-per-skill output. A new TUI test covers ordering, empty descriptions, path shortening and 40-column wrapping, and the real-console probe asserts the styled block and zero provider requests.

- Corrected failed-turn context restoration: rather than truncating by message count after potentially dropping old turns, the runtime restores the exact pre-turn conversation snapshot. A regression fixture above the 256 KiB trimming threshold passes in the current workspace suite.

- Added `jani evidence RUN_ID`, a standalone read-only evidence receipt built from a hash-chain-verified journal. It marks task outcome verification, effect reconciliation and automatic replay as false; CLI and UI regressions cover tampered-journal rejection and verified receipt generation.

- Phase 1B source: multiline bounded terminal composer with Shift+Enter and newline-preserving paste, status-marked tool activities, and audit-only run evidence that explicitly does not independently certify completion.
- Phase 2 foundation: read-only `jani recovery status` flags unfinished runs without unsafe replay; journal inspection checks integrity; Ctrl+C now requests cooperative cancellation at agent-safe boundaries. The current workspace suite and Windows terminal probe exercise recovery inspection, journal integrity and terminal cancellation.

- Failure notices in TUI and line-mode now explicitly distinguish model-context restoration from filesystem or external effects that may survive a failed turn. This is a disclosure improvement, **not** crash recovery or automatic rollback.

- Product excellence programme established with phased UX, recovery, verification, security and extension gates. README now leads with the governed engineering-agent purpose and an explicit supervised-prototype warning.
- First-run terminal now displays four starter options (F1–F4) that only prefill read-only drafts, never execute tools. The configured sandbox is labelled a **profile**, not proof of isolation. Rust TUI tests and the current Windows console probe cover those behaviours.

- The full-screen interface now streams live turn progress instead of waiting for a complete reply. The meta line shows the current step and action, each finished tool adds a `tool` entry with a short summary such as `reading notes.txt`, the answer preview updates as inspected lines become available, and model reasoning is folded behind `Ctrl+T`. OpenAI-compatible providers use `stream: true` with `stream_options.include_usage`; the Anthropic adapter streams `text_delta`, `thinking_delta` and `input_json_delta` blocks and folds thinking. Line mode keeps the non-streaming request path.
- Streaming preserves output governance: previews are inspected before release, only complete lines are emitted, and an unterminated private-key block is withheld from its header. The final assistant message is inspected whole. An answer without newlines therefore appears when the turn completes rather than character by character, and a streaming request cannot be retried after the response headers arrive.
- The transcript is now scrollable and keeps its own focus. The mouse wheel scrolls three lines per notch, `PgUp`/`PgDn` ten, and `Home`/`End` — always with `Ctrl`, or bare when the input is empty — jump to the top or resume following; `Left`/`Right` and cursor movement are unaffected while you are typing. Once the transcript is taller than the viewport Jani reserves the rightmost column for a vertical scrollbar (windows at the 40-column minimum scroll without one) and the tip line reports how many lines sit below the bottom. Following stops as soon as you scroll up and resumes when you return to the last line, so a streaming turn can no longer pull the view away from what you are reading. Mouse capture is now on for the whole session: the pointer scrolls and clicks instead of selecting text, so use the terminal's selection override (Shift in Windows Terminal and most emulators) to copy.
- Each block of model reasoning folds on its own. A thinking block shows a one-line `▸ think` summary; clicking that line expands or folds just that block, and `Ctrl+T` toggles the newest one, including the live block while a turn is still streaming. A block you left open is committed to the transcript still open, so older thinking stays readable after later turns arrive.
- Typing `/` on an empty input now opens an interactive command palette above the input box. It lists every slash command with its usage and summary, filters by name as you type, moves with `Up`/`Down` (wrapping at the ends), runs the highlighted command with `Enter`, fills its name with `Tab`, and closes with `Esc`. `/approval`, which requires an ID, prefills `/approval ` so the ID can be typed; every other command runs immediately through the same checked command path. The palette reads the same command catalog as `/help` and cannot widen the frozen authority.
- The model can now be switched from the palette without restarting. `/model` opens a second-level picker over the profiles in the active settings file; typing filters by profile name or model ID, the current profile is marked, `Up`/`Down` move, `Enter` applies the choice, `Tab` completes, and `Esc` closes. When no profile matches, the typed text is offered as a raw model ID that keeps the current transport and credential. A profile switch applies its model, API base, protocol, output ceiling and credential binding. The selected model is runtime state, not authority: policy, identity, contract, write scopes, executables, sandbox and network stay frozen, and each switch is journaled as an `agent.model` event before it takes effect. Switches with no resolvable credential or a non-HTTPS non-localhost base are refused, and `/model NAME` remains available in line mode.
- The real-console probe now serves SSE. Its fake provider returns a `workspace_read` tool call on the first request and a streamed Markdown answer with reasoning on the second; the probe asserts the live step, the rendered tool activity, formatted bullets, folded reasoning, exactly two key-free streaming requests and exit 0. It now injects console mouse input records as well, so `/help` overflowing the viewport proves the scrollbar and the below-bottom tip, `PgUp` and the wheel prove scrolling, and a synthetic click on the `▸ think` label proves the fold opens.
- Staged Sentinel native hardening for read-only Linux journald/file collection, per-source coverage, bounded persistent spooling, operator mTLS enrollment and revocation, normalized central metrics/logs/incidents, data retention and supervised systemd pilot templates. Source and tests are not a production-certified release.
- Settings now fall back to a user-wide file: Jani reads `--settings` / `JANI_SETTINGS` when given, then `<worktree>/.jani/settings.json`, then `~/.jani/settings.json` (`%USERPROFILE%\.jani\settings.json` on Windows). Only the first file found is used; files are not merged. `JANI_USER_SETTINGS` overrides the user-wide path. A malformed or explicitly named missing file still fails closed, and integration tests point `JANI_USER_SETTINGS` at a nonexistent path so a developer's personal settings cannot change test behavior.
- `.jani/settings.json` now follows Claude Code conventions: optional `$schema`, a top-level `model` selector that is a profile name or a raw model ID, per-profile `protocol` and `maxOutputTokens`, and a `permissions` block with `defaultMode`, `allow`, `ask` and `deny`. The legacy `defaultModel` remains accepted.
- Inline `apiKey` values are now accepted when explicitly configured, alongside `apiKeyEnv`. Values are redacted from `Debug`, diagnostics and errors; `doctor` reports only the source and warns that the file holds a secret. This supersedes the earlier rejection of inline keys.
- Added Claude-style permission rules `Read(./path)`, `Write(./path)` and `Bash(command *)`, enforced in the broker after the work contract and before policy. Evaluation is deny > plan read-only > ask > per-category allowlist. Settings denies are journaled; ask matches require an external `settings-permission` approval; allow rules never widen CLI grants, contract capabilities or signed policy. `bypassPermissions` is refused.
- Added Anthropic Messages transport (`/v1/messages`, `x-api-key`, `anthropic-version`) with tool-call translation, auto-detected for `api.anthropic.com`. `--protocol`, `JANI_PROTOCOL`, `--max-output-tokens` and `JANI_MAX_OUTPUT_TOKENS` override it.
- `doctor` now reports the protocol and API base, the credential source, settings warnings and the compiled permission mode and rule counts.
- Bare `jani` in a terminal now opens a full-screen interactive interface: alternate-screen transcript, multi-line input with history and slash-command completion, a frozen-authority meta line, key hints, a tip line and a status bar. It runs over the same governed session as `jani repl`; line mode remains when stdin or stdout is redirected. With redirected stdin, bare `jani` still prints help, so existing scripts are unchanged.
- The full-screen interface suppresses the per-turn stderr run banner, which would otherwise corrupt the frame. Run identifiers remain available through `/status` and `/inspect`.
- The full-screen interface renders assistant answers as Markdown without new dependencies: headings, emphasis, inline code, bullet and numbered lists, block quotes, rules, tables and fenced code blocks are formatted, with style-aware wrapping and a literal fallback for malformed constructs. A real-console probe serves a Markdown answer and asserts that the screen shows formatted text and no raw `**` or backticks.
- A rolled-back turn is now explicitly marked in both interfaces: after a failed turn the transcript adds a note that the model does not retain it and that the next message continues from the last successful turn, and line mode writes the same note to stderr. The system prompt also tells the model not to claim memory of a rolled-back turn.
- The full-screen editor is Unicode-safe, restores an in-progress draft when walking history, requires a 40x10 terminal and restores the console on exit, error or panic.
- Added `jani repl`, an interactive session over the existing agent loop. One task ID spans the session, each prompt gets its own run ID, and authority (policy, identity, contract, write scopes, executable programs, sandbox and network) is loaded once and frozen. History is trimmed at user-turn boundaries above 256 KiB and tool output is clipped at 64,000 bytes; a failed turn rolls back to its pre-prompt checkpoint.
- REPL slash commands are read-only: `/help`, `/status`, `/skills`, `/inspect [RUN_ID]`, `/budget`, `/approvals`, `/approval ID`, `/new`, `/exit`. None widens authority. `/approval` only attaches a request that a separate verified approver already decided, after reloading the store and rechecking tenant, task, status, expiry and separation of duties.
- Redirected stdin is accepted for automation with a warning; answers go to stdout and the prompt to stderr. The budget ledger is written when a session opens so `/budget` is readable before the first model call.
- Added a root `releases/` channel with a versioned Windows binary, SHA-256 checksum and manifest, plus `install.ps1` for one-command, checksum-verifying installation and `tools/release.ps1` for packaging future releases.
- The installer now asks before adding its install directory to the user `PATH`, defaults to no, never prompts in a non-interactive session, and accepts `-NoPath` to suppress the question or `-AddToPath` to add the entry without prompting.
- `tools/release.ps1` now measures dirty state over tracked build inputs only, records commit, build time and dirty state per artifact, refuses to mix commits within a version, and requires `-Replace` to rebuild an existing target from a different commit.
- Rebuilt the `v0.1.0` Windows artifact from clean build inputs at `c9e3f3b`, so its manifest records `"dirty": false`. The first local build had recorded `"dirty": true` while the release tooling was uncommitted.
- Repackaged the typed `releases/agent/windows/v0.1.0/` artifact from commit `a4bc254` with clean build inputs (SHA-256 `14233b98…`, 9,552,384 bytes); the real-console probe passed twice against the identical bytes, command palette checks included. `install.ps1 -Type agent -NoPath` then installed and re-verified the same SHA-256 over the running session by renaming the image first, so the installed copy and the channel now agree; the open session keeps the previous code until it is restarted. The legacy `releases/v0.1.0/` alias remains the earlier `c9e3f3b` artifact.
- Rewrote the root guides so each has one purpose: evaluation and quick start (README), evidence and gaps (VALIDATION), reporting and guarantees (SECURITY), the contribution gate (CONTRIBUTING) and this record.
- Documented `--settings`, `--model-profile` and `JANI_SETTINGS` in the CLI and configuration references.
- Corrected the contribution instructions to use `jani-claims sync` then `check`; `scan` only prints findings.

## Model profiles and documentation contracts — `8bb3080`, `916b90f`

- Added `.jani/settings.json` with named model profiles, a default profile and a `maxSteps` limit. Precedence is CLI flag > environment > settings > built-in default.
- Added `--settings FILE`, `--model-profile NAME` and the `JANI_SETTINGS` environment variable. Relative paths resolve against the worktree.
- Settings are strict: version 1, camelCase, unknown fields rejected, at most 32 profiles, `maxSteps` 1..80. At that commit an inline `apiKey` was rejected; the Unreleased entry above changes this to accepted-with-warning. `apiKeyEnv` remains the recommended credential source.
- Settings cannot grant write or command authority. `doctor` reports the loaded settings file and profile.
- Added a source-linked documentation contract library, `docs/documentation_manifest.json`, and `python tools/doccheck.py` as a CI gate.

**Compatibility:** the settings file is optional, so existing environment-only setups are unchanged. A profile that names `apiKeyEnv` will not fall back to `JANI_API_KEY` or `OPENAI_API_KEY`; this is intentional, to avoid sending one provider's key to another endpoint.

## Twelve native tools — `ae5fcde`

- Added Index, Context, Planner, Pruner, Checkpoint, Verifier, Impact, Scheduler, Workspace, Evidence, Optimizer and Simulator under `backend/native`.
- Exposed them through `jani native TOOL OPERATION` and the governed `native_tool` broker function. They receive no additional permissions, and test execution still routes through the governed `workspace_command`.

## Fault corpus and release gates — `aa13ef5`, `8582661`

- Added a deterministic 5,000,000-case fault-injection corpus covering 23 subsystem domains, 371 failure hooks, 32 mutation variants and 17 profiles, with sharded output, SHA-256 manifests and a coverage summary.
- Added a readiness evaluator in `synthetic/readiness.py` and tightened release-acceptance evidence requirements.
- CI generates and verifies small samples only. Generating a corpus is **not** execution evidence for Jani.

## Documentation claims audit — `90194a2`

- Added `tools/jani-claims` to check Markdown links, source references and explicit source-backed assertions, and to write discrepancies to `claims/`.
- Added `scan`, `sync`, `check`, `strict` and `queue`. AI-submitted review candidates are never treated as verified facts.

**Compatibility:** changing Markdown can make the claims register stale. Run `cargo run -p jani-claims -- sync` before `-- check`.

## MCP governance — `79b3c5a`, `57f43db`, `fa9ef14`

- Added signed, fail-closed MCP delegation: verified initiator identity and task roles must match an approved server contract, and results require signed authorization and classification receipts.
- Agent invocations of user-only commit tools are always blocked.
- Rejected wildcard grants and unsafe signed endpoints.
- Only the explicitly authorized `tools/call` transport is supported.

## Mirror retrospective critique — `af74c7d`, `f475136`

- Added deterministic review of recorded runs, run comparison and evidence-linked improvement proposals that are saved only on request.
- Added tenant boundaries and journal-tamper tests. Mirror does not execute or self-apply recommendations and has no model-client dependency.

## Workspace, store and attention lifecycle — `c79729b`, `e36f9c9`, `ff4b75a`

- Introduced the virtual Cargo workspace and a transactional attention store.
- Added local `agent`, `alerts` and `wake` commands, delivery leases and replay-safe publish keys. Alerts never authorize or launch tools.
- Aligned package layout with the `backend/*` workspace convention.

## Governed agent vertical slice — `c5017f9`

- Added the broker-mediated agent loop, signed tenant-bound policy and identity, work-contract narrowing, audit journal, persisted budgets, single-use approvals, data classification and fail-closed egress.
- Added the enterprise implementation review documenting what is enforced and what is missing.

## Initial standalone agent — `8f2e70d`, `9cdc3a1`, `cefa29d`

- Added the first standalone skill-aware Rust coding agent with a bounded model/tool loop and scoped writes.
- Added architecture, testing and validation documentation plus the MIT license.
- `809c695` (`v0.1.2`) broadened hardening and tests across configuration, crypto, MCP, Mirror, native tools and attention, and added `Cargo.lock`; `e3d1f18` removed an archived synthetic data file. `70b2790` (`v0.1.1`) was a marker commit with no functional change.

## Compatibility notes

- Rust 1.95.0 is pinned. The root workspace has non-default members, so use `--workspace` for full tests and lint.
- Python 3.11 is required only for synthetic tooling.
- Settings affect model selection, transport, limits and narrowing permission rules only. They never replace `--allow-write`, `--write-path`, `--allow-exec` or signed policy, and an inline key does not change precedence over operator flags.
- Documentation with frontmatter must be registered in the manifest; claims must be synced after Markdown changes.
- No migration path exists from an earlier published release because none has been published.

## Known exclusions

- No executable plugin runtime.
- No portable or certified OS sandbox; Windows and macOS cannot satisfy enterprise isolation profiles.
- No managed identity control plane, key rotation/revocation, encrypted tenant storage or signed execution receipts.
- No durable model checkpoint/resume.
- No certified execution of the five-million-case fault corpus.

## Release discipline

When a release is actually tagged, record at minimum: the commit, binary digests, platform and runner, migration or compatibility implications, security advisories and links to execution evidence. Do not summarize a release with test counts alone. A breaking contract change needs a version note and either a migration path or an explicit refusal path. No release may be labeled enterprise production-ready without the evidence required by [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md).

## Maintenance

Keep this document aligned with implemented code and record uncertainties instead of inventing support.
