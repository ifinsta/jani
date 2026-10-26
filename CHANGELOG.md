---
id: JANI-GOV-002
title: "Changelog"
status: implemented-partial
owner: project
version: 1.3.0
last-reviewed: 2026-10-26
source:
  - README.md
tests: []
---

# Changelog

This page records what changed and what it means for compatibility. It is a development record, not a release certification. Evidence for the current state is in [VALIDATION.md](VALIDATION.md).

## Release status

- **No Git tags exist.** The workspace version in `Cargo.toml` is `0.1.0`.
- `releases/` is a commit-based channel, not a tagged release feed. `releases/v0.1.0/` contains a Windows binary built from commit `c9e3f3b` with clean build inputs; it has not passed release acceptance.
- This page describes commit `c9e3f3b` (2026-10-25). Later changes are listed under Unreleased.
- Two commits have `v0.1.1` and `v0.1.2` in their subject lines. They are development markers from earlier work, not tagged or published releases. Do not cite them as versions.

## Unreleased

- Failure notices in TUI and line-mode now explicitly distinguish model-context restoration from filesystem or external effects that may survive a failed turn. This is a disclosure improvement, **not** crash recovery or automatic rollback.

- Product excellence programme established with phased UX, recovery, verification, security and extension gates. README now leads with the governed engineering-agent purpose and an explicit supervised-prototype warning.
- First-run terminal now displays four starter options (F1–F4) that only prefill read-only drafts, never execute tools. The configured sandbox is labelled a **profile**, not proof of isolation. New Rust TUI tests and an updated Windows console probe cover those behaviours; **this change has not yet passed a fresh Rust build or console probe**.

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
