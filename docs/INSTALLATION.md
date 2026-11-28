---
id: JANI-OPS-003
title: "Installation and initial verification"
status: implemented-partial
owner: backend/server
version: 1.4.0
last-reviewed: 2026-11-25
source:
  - Cargo.toml
  - install.ps1
  - install.sh
  - tools/install_unix.py
  - tools/release.ps1
  - tools/release_signing.py
  - tools/serve_updates.py
  - backend/server/src/update.rs
  - backend/server/src/update/self_replace.rs
tests:
  - backend/server/tests/update_e2e.rs
  - tests/test_release_catalog.py
---

# Installation and initial verification

Use this page to get a runnable `jani` binary and prove it starts without granting any effects. Installation does not configure credentials, write scopes, signed policy or sandbox support; those remain explicit runtime decisions.

## Install a release

On Windows x64, from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 `
  -UpdateKey release-2026:<64_HEX_PUBLIC_KEY>
```

The installer resolves the typed channel, matches an artifact to the host target, verifies its Ed25519-signed `jani.release/v2` manifest and exact SHA-256/size, runs a temporary candidate with provider credentials removed, then copies it to `%LOCALAPPDATA%\Jani\bin`. Typed v2 installation requires Python 3 and OpenSSL 3 with Ed25519 support; no Rust toolchain is required. Repeat `-UpdateKey` for additional trusted signers.

Useful options:

- `-Version v0.1.0` installs a specific release.
- `-InstallDir C:\Tools\Jani\bin` changes the destination.
- `-AddToPath` appends the install directory to the user `PATH` without prompting; a new terminal is required to see it.
- `-NoPath` never asks about or modifies `PATH`.
- `-Target` overrides host detection and must match an artifact in the manifest.
- `-AllowUnsignedLegacy` explicitly permits the frozen unsigned v1 Windows fallback. It never permits an unsigned typed v2 manifest.

When the install directory is not on `PATH`, an interactive run asks before changing the user environment and defaults to no. A non-interactive run with neither flag leaves `PATH` untouched and prints the command for the current session. `-AddToPath` and `-NoPath` cannot be combined. Installing never configures credentials, write scopes or command authority.

Release manifests are signed, but Windows binaries are not Authenticode code-signed. Windows SmartScreen may warn; verify the trusted signer, checksum and source commit, or build from source. Previously published unsigned v0.1.x typed manifests are refused until a publisher re-signs them. See [releases/README.md](../releases/README.md) for the layout and release process.

## Install from source

```powershell
cargo install --path backend/server --locked
jani --version
```

Or build in place and run the workspace binary:

```powershell
cargo build --release -p jani
.\target\release\jani.exe --version
```

Source builds follow the pinned toolchain in `rust-toolchain.toml`. Python is not required unless you run the synthetic tooling.

## Release channel

The canonical release tree is `releases/{type}/{platform}/{version}/`, where component type is `agent`, `client` or `gateway`, and platform is `windows`, `macos` or `linux`. Typed channels such as `releases/agent/windows/latest.json` choose the component/platform version. The frozen `releases/latest.json` points only at the legacy Windows agent version for old installers. Each typed version directory contains the platform binary, its `.sha256` file and a manifest recording the version, source commit, build time and per-artifact digest, size, commit and dirty state. `dirty` covers tracked build inputs only, so documentation or tooling edits do not make a binary dirty. To publish a new release, update the workspace version and changelog, commit the source, then run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\release.ps1 `
  -UpdateKeyPath C:\Keys\jani-release-private.pem -UpdateKeyId release-2026
```

Commit the new typed version directory and its component/platform `latest.json`. The publisher signs the manifest before advancing the channel and prints the public `ID:HEX` trust root; keep the private key outside the repository. Do not move the old v0.1.0 Windows agent alias; existing installers still depend on it. Use `-Type agent`, `-Type client` or `-Type gateway` with the release script; the last two fail until their actual Cargo packages are implemented. The script refuses when tracked build inputs differ from `HEAD` unless `-AllowDirty` is passed, refuses to label a host build as another target, refuses to add a target from a different commit, and requires `-Replace` to rebuild an existing target from a different commit.

On Linux/macOS, where a matching signed release binary exists, use `sh ./install.sh --type agent --update-key release-2026:<64_HEX_PUBLIC_KEY>` or install from source. The local Unix installer requires Python 3 and OpenSSL 3 for signed-manifest and checksum verification; no Python runtime is bundled into the Rust agent. Current repository publishes no Linux/macOS binaries.

## Self-distributed updates

Installed agents can pull the same typed channel themselves, either on demand or continuously:

```powershell
$trust = 'release-2026:<64_HEX_PUBLIC_KEY>'
jani update check --source https://releases.example.com/ --update-key $trust
jani update install --source https://releases.example.com/ --update-key $trust
jani update install --source https://releases.example.com/ --update-key $trust --to C:\path\to\coding-agent\jani.exe
jani update watch --source https://releases.example.com/ --update-key $trust --interval 300
```

`check` verifies the signed channel manifest and reports each target's status without downloading an artifact or executing an existing target. Known versions come from the running executable, byte-identical copies or an exact signed-artifact match. Other existing files report `unknown-version` and require `--force`. `install` verifies the signature, size and SHA-256, and runs the candidate's `--version` with configured credentials removed before replacement. For additional targets it records the signed manifest in the state directory so future invocations can verify the installed version, including after restart. `install` also writes a diagnostic receipt under `<state-dir>/updates/`. The default target is the running executable. Repeat `--to` for additional local binaries. On Windows, a non-self target in use can fail with a sharing error. A Windows self-update reports `restartRequired` and retains a verified sibling backup for offline recovery if needed. `watch` polls at the interval and exits after a self-update or a recovery-required result. Set `--cycles` to limit the loop or zero to keep watching until one of those outcomes. [Updater state, trust, and recovery](CLI_REFERENCE.md)

The origin must be HTTPS, or loopback HTTP for local development. Redirects are refused, metadata and artifacts are size-bounded, and a signature, size or digest mismatch is rejected before any target is touched. Trust roots come only from repeatable `--update-key` or comma-separated `JANI_UPDATE_KEYS`; the origin and worktree settings cannot supply them. Unsigned or unknown-key manifests are refused.

## Configure and verify

```powershell
$env:JANI_API_KEY = "YOUR_KEY"
$env:JANI_MODEL = "YOUR_TOOL_CALLING_MODEL"
jani doctor
jani sandbox doctor
jani --print "Summarize this repository. Do not edit anything."
```

`JANI_API_BASE` defaults to `https://api.openai.com/v1`. Keys belong in environment variables or a secret manager, never in committed files. A named profile in `.jani/settings.json` can supply the model and the `apiKeyEnv` variable; see [CONFIGURATION_REFERENCE.md](CONFIGURATION_REFERENCE.md).

Start read-only. Enable writes only with explicit relative scopes, and enable commands only when you are prepared to execute project code:

```powershell
jani --print "Implement the change, then run the tests" `
  --allow-write --write-path src --write-path tests --allow-exec
```

The Windows sandbox bounds a process tree through a Job Object; it is not an OS sandbox. Prefer a disposable checkout or container for command execution.

## Limits

- A committed artifact proves a binary was built, not that release acceptance evidence exists. See [RELEASE_ACCEPTANCE.md](RELEASE_ACCEPTANCE.md) and [VALIDATION.md](../VALIDATION.md).
- Only targets committed under `releases/` are installable without a Rust toolchain. Other platforms need a source build or a new release.
- There is no managed identity, OS signing certificate or rollback service behind these installers. `jani update` verifies an explicitly trusted Ed25519 release signature, HTTPS delivery, artifact digest and credential-scrubbed candidate execution, but a replaced binary is not rolled back automatically. Installation remains a verified local file copy.
- Building from source does not supply trusted policy roots or enterprise isolation.

## Acceptance

- A valid artifact installs and reports its version; a tampered artifact is refused before any file is copied.
- A fresh install still denies writes and command execution until explicit scopes are supplied.

## Maintenance

Review exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
