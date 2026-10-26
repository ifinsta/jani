## Sentinel hardening: 2026-10-25 (unreleased, non-production)

Native source staged for Linux file/journald collectors, standardized CPU/memory/disk/process metrics, bounded systemd inventory, 64 MiB identity-bound offline spool, runtime certificate revocation, normalized transactional SQLite tables, tenant-specific incidents and configurable 1–90 day data retention. Dedicated systemd service examples and security regression tests are included.

**Executed locally in this session:** `python -m unittest discover -s tests -v` within the original JSMT Python reference: 15 passed; local registry schema tests: 3 passed; ephemeral CA-signed client-auth certificate enrollment/revocation CLI: passed. This is evidence of the Python reference and operator tool logic, not native Rust compilation.

**Unpassed release gates:** Rust toolchain absent in the execution environment; cargo fmt, clippy, build, workspace tests and the new Rust probe/gateway separate-process mTLS test have not executed. Prior GitHub Actions jobs are failing without assigned runners or steps. Real-host log inventory, Windows/macOS/container adapters, managed PKI/renewal, encrypted-at-rest production stores, HA failover, load/soak, signing and security review remain outstanding. No new binaries published.

See [Sentinel operational gates](docs/SENTINEL_OPERATIONS.md).

# Validation status

**Verdict: supervised local prototype. Not production-approved and not enterprise-certified.**

This page answers one question: *what has actually been executed, and what does that evidence allow you to claim?* It is a snapshot, not a permanent certification. Source code and executable tests are the authority; a passing count here does not extend to platforms or providers that were never run.

Snapshot: 2026-10-26, Windows `x86_64-pc-windows-msvc`, Rust 1.95.0 pinned by `rust-toolchain.toml`.

## What is proven

| Gate | Command | Observed result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | Clean |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | Zero warnings |
| Tests | `cargo test --workspace --all-targets` | 225 passed, 0 failed: 199 unit, 24 CLI/integration, 2 fake-provider E2E |
| Release build | `cargo build --release -p jani` | Succeeds; `jani --version` and `jani --help` run |
| CLI smoke | `jani doctor`, `jani sandbox doctor` | Report worktree, skills, model, settings source, permissions and sandbox support |
| Architecture gate | `cargo run -p jani-check` | Passes |
| Documentation manifest | `python tools/doccheck.py` | 57 normative documents and the 12 mandatory core contracts validate |
| Claims register | `cargo run -p jani-claims -- check` | Register current; 0 unresolved deterministic findings |
| Synthetic corpus | `python -m synthetic.generate generate` / `verify` | 5,000,000 cases generated and integrity-verified locally |
| Native corpus | `python -m synthetic.native_tools generate` / `verify` | Generated and verified; CI exercises 2,400 cases |
| Release installer | `powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1` | `v0.1.0` installs after SHA-256 verification and reports its version; the trial ran against the earlier `c9e3f3b` artifact, and the typed manifest now records the repackaged `a4bc254` build (SHA-256 `14233b98…`); a non-interactive install left the user `PATH` unchanged; a tampered copy and conflicting PATH flags are refused before installation |
| CI matrix | `.github/workflows/ci.yml` | Ubuntu and Windows jobs run formatting, claims, docs, clippy, tests and release build |

### Security behavior covered by negative tests

The suite does not merely exercise happy paths. Executed tests deny or reject:

- Writes without `--write-path`, out-of-scope paths, traversal, symlink components and protected credential paths.
- Unsigned, expired, wrong-tenant or tampered policy and identity assertions; role-scoped rules; empty and wildcard egress allowlists.
- Secret-classified content leaving through model requests, even when redaction is disabled.
- Unapproved external writes and high-risk actions without a single-use, exactly bound approval.
- Journal tampering, broken hash chains and budget ledgers that would otherwise widen across restarts.
- Missing isolation profiles and unavailable sandboxes rather than falling back silently.
- MCP forged, expired, missing-role or wildcard delegations, tampered response receipts and agent use of user-only commit tools.
- Unknown settings fields, `bypassPermissions`, malformed permission rules and unsupported rule tools; settings allowlists that would otherwise permit a command without `--allow-exec`; settings `deny` and plan mode refusing reads, writes or commands while permitting allowed reads.
- Inline API keys accepted but never echoed or `Debug`-printed; profile credentials never silently borrowing another provider's generic environment key.
- Credential inheritance by child processes.
- REPL prompt arguments and a fixed `--run`; approvals attached by the session principal, issued for another task, or already expired.
- TUI edge cases: Unicode editing and deletion, history draft restoration, command completion, word wrapping and long-word splitting, role labelling, welcome/conversation/busy frames and terminals below the minimum size; live tool entries, `Ctrl+T`, and streaming preview rendering; wheel, `PgUp`/`PgDn` and `Home`/`End` scrolling with the scrollbar and below-bottom tip, and per-block thinking folds that open and close independently of the newest one; slash-command palette opening on `/`, prefix filtering, wrapping selection, Tab fill, Esc and slash-deletion close, argument prefill for `/approval`, suppression while a turn runs, and end-to-end execution of the selected `/new` through the real command path; the `/model` picker listing settings profiles with the current one marked, filtering by name, completing with Tab, offering a raw model ID only when nothing matches, switching the agent and meta line end to end, and staying closed while a turn runs.
- Streaming transport: OpenAI text/reasoning/tool-call fragments and usage; Anthropic text/thinking/tool-input blocks and usage; stream error events; line-buffered preview release; unterminated private-key withholding; refusal when a released preview prefix changes.

The two fake-provider E2E tests run the real CLI against a temporary worktree and assert that the API key never appears in a provider request, the journal chain verifies and the persisted budget ledger is correct. The REPL test additionally proves that the second provider request carries the first turn's question and answer, that both turns share one task ID but use distinct run IDs, and that the budget accumulates across turns.

## Reproduce it

Run from the repository root on a disposable checkout:

```bash
cargo fmt --all -- --check
cargo run -p jani-check
cargo test -p jani-claims
python tools/doccheck.py
cargo run -p jani-claims -- check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release -p jani
```

Optional synthetic evidence (Python 3.11; outputs stay out of Git):

```bash
python -m unittest discover -s synthetic/tests -v
python -m synthetic.generate generate --output synthetic/generated/full_5m --count 5000000
python -m synthetic.generate verify --output synthetic/generated/full_5m
python -m synthetic.native_tools generate --output synthetic/generated/native_5m --count 5000000
python -m synthetic.native_tools verify --output synthetic/generated/native_5m
```

Optional Windows console evidence for the full-screen terminal (manual, not a CI gate):

```bash
python tools/tui_console_probe.py target/release/jani.exe target/tui-run
```

Record the commit SHA, platform, runner identity and artifact digests if you rely on these results. A rerun on another platform is new evidence, not a formality.

## What the evidence does not cover

- **No live model session.** No provider key or real coding task was supplied. The E2E test uses a deterministic fake provider. Provider behavior, rate limits, billing and tool-call quality remain untested. The Anthropic Messages transport is covered by request/response translation tests; no live Anthropic endpoint was called.
- **Interactive-terminal evidence is console-level, not a terminal emulator.** Automated tests drive line-mode REPL through piped stdin and cover the full-screen editor, layout and Markdown renderer with ratatui's test backend. On 2026-10-25, [tools/tui_console_probe.py](tools/tui_console_probe.py) ran the Windows x64 release binary under a real console: it allocated a console, bound the child's streams to `CONIN$`/`CONOUT$`, injected keys with `WriteConsoleInputW` and read the active screen buffer. That run wrote a Claude-style `.jani/settings.json` with `permissions` into the probe worktree, showed the alternate-screen welcome frame (logo, prompt, meta line with `probe-model`, key hints), executed `/status` and read the frozen authority including `Project permissions: mode=default allow=1 ask=1 deny=1`, sent a prompt to a local fake OpenAI-compatible provider, observed the working state and the rendered answer, counted one turn, recorded exactly one provider request containing no API key, and exited 0 with `PROBE_RESULT=PASS`. On 2026-10-26 the probe was rerun against the release build with the Markdown changes (SHA-256 `ea3cb340…`): the fake provider returned a bold Hello message plus two bullet items, and the screen showed `Hello from the fake provider.` and `• alpha item` / `• beta item` with no `**` or backticks, still one key-free request and exit 0. Later that day the probe was updated for streaming and run against the release build (SHA-256 `bc8b4d69…`): the fake provider sent a `workspace_read` tool call on the first SSE request and a streamed Markdown answer with reasoning on the second. The screen showed the live `step 1/20` activity, a `tool` entry reading `reading notes.txt`, the folded `1 reasoning line` summary, rendered bullets and no raw Markdown. It recorded exactly two streaming requests (`"stream": true` in both), neither containing the API key, and exited 0 with `PROBE_RESULT=PASS`. The probe then gained console mouse-input records and was rerun against the scrolling release (commit `9980b50`, SHA-256 `fd9fb119…`; first the pre-commit build of the same sources, then `releases/agent/windows/v0.1.0/jani-0.1.0-x86_64-pc-windows-msvc.exe`): `/help` pushed the transcript past the 40-row viewport, so the frame showed the `║` track and `█` thumb in the reserved rightmost column; an injected `PgUp` produced `10 lines below the bottom · end or wheel down resumes following`; eight wheel-up records moved the view to the first line, bringing the `/status` block's `Jani v0.1.0` into frame; a click on the `▸ think` row expanded that one block to `checking the fake provider`; and twelve wheel-down records resumed following, clearing the tip. It exited 0 with `PROBE_RESULT=PASS`. Two harness bugs had to be fixed for that: a mouse record is input type `2`, not `4` (type `4` is a window-buffer-size record, which crossterm turned into a resize event the UI ignores), and the vertical wheel is `dwEventFlags 0x0004`, not SendInput's `0x0008` (the horizontal wheel) — with either mistake the wheel silently did nothing, which is why the first two runs reported `wheel_scroll_up: False`. A first run of the typed artifact then failed on `click_expands_thinking`: the settle helper accepted a frame captured while wheel records were still queued, so the `▸ think` label was not yet on screen when the click row was computed. The probe now drains the burst before reading that row, and the typed artifact passed twice in a row. On the same day the probe gained slash-command palette checks and passed twice against the palette build (SHA-256 `14233b98…`, 9,552,384 bytes; the clean repackage from commit `a4bc254` reproduced the same bytes): typing `/` drew the bordered `commands` modal with all ten entries, `st` narrowed it to `/status` and removed `/budget`, `Esc` closed it, a reopened palette plus `Down` selected `/status` and Enter appended its authority note with the modal gone, and `/new` typed through the palette cleared the transcript to the `/new` command and its `Conversation cleared` note. It exited 0 with `PROBE_RESULT=PASS`. On the same day the probe gained model-picker checks and passed twice against the release build from the working tree (SHA-256 `ff515b549d641d9a80e9ecaaab64464b664bc4f6548e2aebec293d9c26e53493`, 9,576,448 bytes): its settings defined `probe` and `alt` profiles on the fake provider; typing `/model ` drew the bordered `model` modal with `alt-model` and `probe-model`, a `current` marker on the active profile and the `enter switch` footer; `al` filtered the rows to `alt-model` without `probe-model`; Enter switched the live session and `/status` then reported `Model: alt-model (profile alt)`; a typed `custom-probe-model` produced the raw-ID row with `use as a model ID`, Enter switched without a profile, and `/status` reported `Model: custom-probe-model`. Both switches ran through the `agent.select_model` path whose journaling is asserted by the unit test, the provider request count stayed at two, and the run exited 0 with `PROBE_RESULT=PASS`. Permission *enforcement* is proven by broker tests, not by this frame check. The probe is retained, but it is a manual harness, not a CI gate. A real terminal emulator is still untested: resize, paste, scrollback, color fidelity, drag selection now that mouse capture is on, Ctrl+C during a turn and streaming behavior in Windows Terminal, Linux or macOS remain unverified.
- **No Linux execution on this host.** bubblewrap confinement and the `cfg(unix)` test path were not run here. CI targets Ubuntu, but a green CI job is not a penetration test.
- **No sandbox-escape or crash-recovery trial.** Confinement is asserted by selection and refusal tests, not by adversarial escape attempts.
- **No executed 5M corpus.** Generating and verifying inputs proves corpus integrity only. There are no per-case Jani execution receipts, so the corpus is not evidence of runtime correctness.
- **Release installer coverage is narrow, and the installed copy was manually placed.** `releases/v0.1.0/` (the legacy alias) still contains the artifact built from commit `c9e3f3b` (SHA-256 `860d934d…`). For this settings/permissions work, a local release build from the dirty worktree (SHA-256 `6405485163971ed2135c0cdc6c33d7905b5df0a6ba2473ccf4371f2b57d4d835`) was copied to `%LOCALAPPDATA%\Jani\bin\jani.exe` on 2026-10-26; `jani doctor --repo C:\Users\user\CascadeProjects\ifinpay` then reported `Settings: C:\Users\user\.jani\settings.json (profile deepseek)` and `Model: deepseek-chat`, proving the user-wide fallback. It was later replaced by the Markdown/rollback build (SHA-256 `ea3cb340d4caf40dfbb9f66431df477fb5cfefcc2df55dfacd68a966d36cfa45`); because the previous binary was still running, it was renamed to `jani.exe.old` rather than replacing a locked file, then removed once that session exited. The streaming build (SHA-256 `bc8b4d6956900ac39caea77e79a8c5e987937afb701e4c4c4f6632f6adb21a27`, 9,532,416 bytes) then replaced the installed copy; the stale `jani.exe.old` from the earlier rename was removed after confirming it was not the running image. The typed channel `releases/agent/windows/v0.1.0/` was then repackaged from commit `feaed0e` with clean build inputs and the same SHA-256, so the installed bytes and the typed artifact agreed. The scrolling release (commit `9980b50`, SHA-256 `fd9fb1193c00a64719cc93c4a33e398b279d0d0723497bcbfc56170492f8847c`, 9,533,952 bytes) replaced the typed artifact and was then installed over that session: the running image was renamed to release the file name, `install.ps1 -Type agent -NoPath` copied and verified the SHA-256 and the install directory was already on `PATH`, so the non-interactive branch ran without prompting; the renamed copy was unlinked while the session kept its mapped image, so `jani.exe` on disk is the new build and the open session (PID 25084) continues on the previous code until it is restarted. The installed copy had also been probed before installation: `releases/agent/windows/v0.1.0/jani-0.1.0-x86_64-pc-windows-msvc.exe` passed the console probe twice. The palette build (commit `a4bc254`, SHA-256 `14233b98fac0da08fe36d5cc457941eeea0a3b6f4698bda1009b888ad46fa9f7`, 9,552,384 bytes) then replaced the typed artifact and was installed the same way over the next running session: the image (PID 15636) was renamed aside, `install.ps1 -Type agent -NoPath` verified the SHA-256, and the renamed copy was removed after the install while the session kept its mapped image. The installed binary reports `jani 0.1.0`; the open session continues on the previous code until it is restarted. The interactive yes branch was not tested. There is no code signing, no clean-machine test, no Linux artifact install and no auto-update or rollback test.
- **No independent signoff.** All evidence here was produced locally or in CI without an external reviewer. See the receipt and reviewer fields in [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md).
- **No enterprise controls.** There is no IdP/SCIM, policy administration, key rotation/revocation, encrypted tenant storage, signed execution receipt, plugin runtime or portable OS sandbox. See the [implementation review](docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md).

## Release decision

A missing critical gate is a blocker, not a caveat. The only defensible current use is a supervised, local, ideally read-only evaluation with disposable worktrees. Do not describe this build as enterprise production-ready or as an OS sandbox on Windows or macOS. Release criteria and required evidence fields are defined in [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md); testing workflow and trial coverage are in [docs/TESTING.md](docs/TESTING.md).

Keep this page current when gates, platforms or guarantees change. Never convert "generated", "planned" or "proposed" into "passed" without an execution record.
