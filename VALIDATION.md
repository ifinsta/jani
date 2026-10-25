## Sentinel hardening: 2026-10-25 (unreleased, non-production)

Native source staged for Linux file/journald collectors, standardized CPU/memory/disk/process metrics, bounded systemd inventory, 64 MiB identity-bound offline spool, runtime certificate revocation, normalized transactional SQLite tables, tenant-specific incidents and configurable 1–90 day data retention. Dedicated systemd service examples and security regression tests are included.

**Executed locally in this session:** `python -m unittest discover -s tests -v` within the original JSMT Python reference: 15 passed; local registry schema tests: 3 passed; ephemeral CA-signed client-auth certificate enrollment/revocation CLI: passed. This is evidence of the Python reference and operator tool logic, not native Rust compilation.

**Unpassed release gates:** Rust toolchain absent in the execution environment; cargo fmt, clippy, build, workspace tests and the new Rust probe/gateway separate-process mTLS test have not executed. Prior GitHub Actions jobs are failing without assigned runners or steps. Real-host log inventory, Windows/macOS/container adapters, managed PKI/renewal, encrypted-at-rest production stores, HA failover, load/soak, signing and security review remain outstanding. No new binaries published.

See [Sentinel operational gates](docs/SENTINEL_OPERATIONS.md).

# Validation status

**Verdict: supervised local prototype. Not production-approved and not enterprise-certified.**

This page answers one question: *what has actually been executed, and what does that evidence allow you to claim?* It is a snapshot, not a permanent certification. Source code and executable tests are the authority; a passing count here does not extend to platforms or providers that were never run.

Snapshot: 2026-10-25, Windows `x86_64-pc-windows-msvc`, Rust 1.95.0 pinned by `rust-toolchain.toml`.

## What is proven

| Gate | Command | Observed result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | Clean |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | Zero warnings |
| Tests | `cargo test --workspace --all-targets` | 170 passed, 0 failed: 147 unit, 21 CLI/integration, 2 fake-provider E2E |
| Release build | `cargo build --release -p jani` | Succeeds; `jani --version` and `jani --help` run |
| CLI smoke | `jani doctor`, `jani sandbox doctor` | Report worktree, skills, model, settings source, permissions and sandbox support |
| Architecture gate | `cargo run -p jani-check` | Passes |
| Documentation manifest | `python tools/doccheck.py` | 54 normative documents and the 12 mandatory core contracts validate |
| Claims register | `cargo run -p jani-claims -- check` | Register current; 0 unresolved deterministic findings |
| Synthetic corpus | `python -m synthetic.generate generate` / `verify` | 5,000,000 cases generated and integrity-verified locally |
| Native corpus | `python -m synthetic.native_tools generate` / `verify` | Generated and verified; CI exercises 2,400 cases |
| Release installer | `powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1` | `v0.1.0` (manifest commit `c9e3f3b`, clean build inputs) installs after SHA-256 verification and reports its version; a non-interactive install left the user `PATH` unchanged; a tampered copy and conflicting PATH flags are refused before installation |
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
- TUI edge cases: Unicode editing and deletion, history draft restoration, command completion, word wrapping and long-word splitting, role labelling, welcome/conversation/busy frames and terminals below the minimum size.

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
- **Interactive-terminal evidence is console-level, not a terminal emulator.** Automated tests drive line-mode REPL through piped stdin and cover the full-screen editor and layout with ratatui's test backend. On 2026-10-25, [tools/tui_console_probe.py](tools/tui_console_probe.py) ran the Windows x64 release binary under a real console: it allocated a console, bound the child's streams to `CONIN$`/`CONOUT$`, injected keys with `WriteConsoleInputW` and read the active screen buffer. That run wrote a Claude-style `.jani/settings.json` with `permissions` into the probe worktree, showed the alternate-screen welcome frame (logo, prompt, meta line with `probe-model`, key hints), executed `/status` and read the frozen authority including `Project permissions: mode=default allow=1 ask=1 deny=1`, sent a prompt to a local fake OpenAI-compatible provider, observed the working state and the rendered answer, counted one turn, recorded exactly one provider request containing no API key, and exited 0 with `PROBE_RESULT=PASS`. Permission *enforcement* is proven by broker tests, not by this frame check. The probe is retained, but it is a manual harness, not a CI gate. A real terminal emulator is still untested: resize, paste, scrollback, color fidelity, Ctrl+C during a turn, streaming output, and rendering in Windows Terminal, Linux or macOS remain unverified.
- **No Linux execution on this host.** bubblewrap confinement and the `cfg(unix)` test path were not run here. CI targets Ubuntu, but a green CI job is not a penetration test.
- **No sandbox-escape or crash-recovery trial.** Confinement is asserted by selection and refusal tests, not by adversarial escape attempts.
- **No executed 5M corpus.** Generating and verifying inputs proves corpus integrity only. There are no per-case Jani execution receipts, so the corpus is not evidence of runtime correctness.
- **Release installer coverage is narrow, and the installed copy is not the committed release artifact.** `releases/v0.1.0/` still contains the artifact built from commit `c9e3f3b` (SHA-256 `860d934d…`). For this settings/permissions work, a local release build from the dirty worktree (SHA-256 `ab2b8605541b9c8985eb7349b1ddba4be9afca8796bdc3bcf84b6676b5c7db90`) was copied to `%LOCALAPPDATA%\Jani\bin\jani.exe` and the console probe against that installed path passed. Re-running `install.ps1` would replace it with the older committed artifact. The non-interactive PATH branch was tested; the interactive yes branch was not. There is no code signing, no clean-machine test, no Linux artifact install and no auto-update or rollback test.
- **No independent signoff.** All evidence here was produced locally or in CI without an external reviewer. See the receipt and reviewer fields in [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md).
- **No enterprise controls.** There is no IdP/SCIM, policy administration, key rotation/revocation, encrypted tenant storage, signed execution receipt, plugin runtime or portable OS sandbox. See the [implementation review](docs/jani_enterprise_proposals/IMPLEMENTATION_REVIEW.md).

## Release decision

A missing critical gate is a blocker, not a caveat. The only defensible current use is a supervised, local, ideally read-only evaluation with disposable worktrees. Do not describe this build as enterprise production-ready or as an OS sandbox on Windows or macOS. Release criteria and required evidence fields are defined in [docs/RELEASE_ACCEPTANCE.md](docs/RELEASE_ACCEPTANCE.md); testing workflow and trial coverage are in [docs/TESTING.md](docs/TESTING.md).

Keep this page current when gates, platforms or guarantees change. Never convert "generated", "planned" or "proposed" into "passed" without an execution record.
