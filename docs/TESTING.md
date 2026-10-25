# Testing and acceptance plan

## Verification gates

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

The workspace root lists `backend/server` under `default-members`, so the
`--workspace` flag is required to exercise every crate.

Expected automated coverage in this build:

- 147 unit tests across config, policy, identity, data, broker, journal, approvals,
  budget, sandbox, process, workspace, skills, schema, plugins, project settings,
  Claude-style permission matching, model transport, native tools, mirror reports,
  the interactive terminal editor and renderer, and the check/claims tooling.
- 23 CLI and integration tests: 14 CLI tests including the shipped work-contract
  example, settings precedence, inline-credential redaction, profile credential
  binding, top-level `model` selection, `permissions` diagnostics, the guarantee
  that `--version`/`--help` need no provider credentials and the interactive
  default, plus MCP gateway, mirror CLI, native tool and end-to-end suites.
- Broker tests prove settings `deny`, `ask` and allowlists narrow contract and
  policy decisions; plan mode blocks writes and commands while allowing reads.
- Anthropic transport is verified by request and response translation fixtures.
  It has not been exercised against a live Anthropic endpoint.
- 2 end-to-end tests that start a fake OpenAI-compatible provider and run the real
  CLI through policy, gateway, broker, audit and budget accounting. One covers
  conversation continuity across two turns in one task.

The E2E test removes `OPENAI_API_KEY`, `JANI_POLICY` and `JANI_IDENTITY` from the
child environment so it exercises local-unmanaged mode deterministically.

## Mandatory integration trials (disposable clone only)

| Trial | Expected outcome |
|---|---|
| `jani --version`, `jani --help`, `jani skills`, `jani doctor` | Finite output, no model call required |
| `.jani/settings.json` then `--model-profile NAME` | `doctor` reports the profile; CLI flags override env, env overrides settings |
| Settings with an unknown field or malformed rule | Refused before work |
| Settings with an inline `apiKey` | Accepted, reported as `settings (inline)` with a warning; the value never appears in output |
| Settings permissions `deny` / `ask` / `allow` | Deny is journaled; ask requires an external `settings-permission` approval; allow never widens CLI, contract or policy |
| `jani sandbox doctor`, `jani sandbox check` | Backend availability and confinement claims match the host; no false confinement |
| `jani policy explain`, `jani policy check --action ... --resource ...` | Deterministic decision without a model call |
| `jani contract validate docs/schemas/work-contract.example.json` | `ok: true` |
| Missing API key | Jani exits with a clear error before the model request |
| Read-only run | No writes or commands occur |
| `--allow-write --write-path src` | Only non-symlink files under `src/` are writable |
| `../`, `.git/`, `.agents/`, `.env` and symlink escape | Refused |
| Write outside the approved scope | Refused |
| `workspace_command` with shell, arbitrary executable, `git -C`, `git --no-index` | Refused |
| `cargo clippy --fix`, `cargo fmt` without `--check` | Refused |
| `git add -A` / `-u` or directory paths | Refused |
| `git commit` with pre-staged files outside scope | Refused before commit |
| Command timeout and output caps | Process is bounded; oversized output is truncated |
| Transient provider error (408/429/5xx) | Limited retry, then a clear failure |
| Two `SKILL.md` files with the same name | Earlier discovery directory wins |
| Provider credentials in child environment | Not inherited |
| Enterprise mode without `--policy` or `--identity` | Refused before any work |
| Signed policy with `defaults.action` other than `deny` | Refused |
| Expired policy, wrong tenant, bad signature | Refused |
| Policy rule with `roles` | Refused at load |
| Empty provider/region/purpose allowlist | Model egress denied |
| Secret in decoded JSON string | Redacted before egress; secret content never transmitted |
| Unknown or malformed tool call | Denied and journaled |
| Approval self-approval, wrong role, wrong payload, reuse | Refused |
| Corrupt or partially consumed budget ledger | Fails closed or adopts the stricter limit |
| `audit verify` after a completed run | `ok: true` |
| Manual journal edit | `audit verify` fails |
| Unsigned, tampered or native-runtime plugin manifest | Registry load refused |
| `EnterpriseRestricted` profile on Windows | Refused; Job Object never claims filesystem/network confinement |
| `EnterpriseRestricted` profile on Linux with bwrap available | bubblewrap plan; receipt reports filesystem and network confinement |
| Linux and Windows builds | Toolchain gates pass on both |
| `python tools/tui_console_probe.py target/release/jani.exe target/tui-run` (Windows console) | Full-screen frames render, `/status` runs, a fake-provider turn completes, exactly one request carries no API key, exit 0 and `PROBE_RESULT=PASS` |

The console probe is a manual Windows trial, not a CI gate. It proves the
rendered buffer in a real console, but not a terminal emulator: resize, paste,
scrollback, color fidelity and Ctrl+C during a turn remain manual, and no
full-screen run has been performed on Linux or macOS.

## Signed-file trials

There is no issuer CLI yet, so signed policy, identity and plugin-manifest trials
must use files produced by an external tool that reproduces this binary's
serialization. The unit tests sign with test keys in-process. Until an issuer CLI
and a canonical JSON form exist, treat cross-vendor signing as unverified.

## Safety

An allowlisted test runner can still execute malicious project code. Use a
disposable checkout and OS/container isolation for untrusted repositories. On
Windows, Job Objects bound the process tree only; do not represent that as
filesystem or network confinement. macOS has no backend in this build.

## Adversarial synthetic corpus and production readiness

The `synthetic/` package maps 371 failure hooks to Jani source domains and generates
5,000,000 deterministic scenario descriptors across security, permissions,
MCP, model gateways, tools, process confinement, storage, recovery and claims.
Full-corpus generation is run separately from pull-request CI; PR CI uses a
small sample for schema, determinism, checksum and fault-distribution tests.

```bash
python -m unittest discover -s synthetic/tests -v
python -m synthetic.generate generate --output synthetic/generated/full_5m --count 5000000
python -m synthetic.generate verify --output synthetic/generated/full_5m
python -m synthetic.readiness --corpus synthetic/generated/full_5m
```

A corpus is **not** a passing runtime test. Do not mark any scenario passed
without executing the Jani binary or directly instrumented Rust subsystem and
recording a verifiable oracle receipt. The initial supported CLI probe adapter
is in `synthetic/runner.py`; unsupported hooks are explicitly SKIPPED. A
production readiness verdict requires all mandatory gates plus actual full
runtime evaluation and independent security signoff; the baseline evaluator
returns nonzero when evidence is absent.
