# Testing and acceptance plan

## Verification gates

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Expected automated coverage in this build:

- 61 unit tests across config, policy, identity, data, broker, journal, approvals,
  budget, sandbox, process, workspace, skills, schema and plugins.
- 5 CLI tests, including the shipped work-contract example and the guarantee that
  `--version`/`--help` need no provider credentials.
- 1 end-to-end test that starts a fake OpenAI-compatible provider and runs the real
  CLI through policy, gateway, broker, audit and budget accounting.

The E2E test removes `OPENAI_API_KEY`, `JANI_POLICY` and `JANI_IDENTITY` from the
child environment so it exercises local-unmanaged mode deterministically.

## Mandatory integration trials (disposable clone only)

| Trial | Expected outcome |
|---|---|
| `jani --version`, `jani --help`, `jani skills`, `jani doctor` | Finite output, no model call required |
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
