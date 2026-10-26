---
id: JANI-OPS-003
title: "Installation and initial verification"
status: implemented-partial
owner: backend/server
version: 1.3.0
last-reviewed: 2026-10-27
source:
  - Cargo.toml
  - install.ps1
  - tools/release.ps1
  - tools/serve_updates.py
  - backend/server/src/update.rs
tests:
  - backend/server/tests/update_e2e.rs
---

# Installation and initial verification

Use this page to get a runnable `jani` binary and prove it starts without granting any effects. Installation does not configure credentials, write scopes, signed policy or sandbox support; those remain explicit runtime decisions.

## Install a release

On Windows x64, from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

The installer resolves `releases/latest.json`, matches an artifact to the host target, verifies the SHA-256 digest from `releases/agent/windows/<version>/manifest.json`, refuses a missing binary or mismatch, copies the binary to `%LOCALAPPDATA%\Jani\bin`, and runs `jani --version`. No Rust toolchain is required when a matching artifact exists.

Useful options:

- `-Version v0.1.0` installs a specific release.
- `-InstallDir C:\Tools\Jani\bin` changes the destination.
- `-AddToPath` appends the install directory to the user `PATH` without prompting; a new terminal is required to see it.
- `-NoPath` never asks about or modifies `PATH`.
- `-Target` overrides host detection and must match an artifact in the manifest.

When the install directory is not on `PATH`, an interactive run asks before changing the user environment and defaults to no. A non-interactive run with neither flag leaves `PATH` untouched and prints the command for the current session. `-AddToPath` and `-NoPath` cannot be combined. Installing never configures credentials, write scopes or command authority.

Release binaries are not code-signed. Windows SmartScreen may warn; verify the checksum and the source commit in the manifest, or build from source. See [releases/README.md](../releases/README.md) for the layout and the release process.

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
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\release.ps1
```

Commit the new typed version directory and its component/platform `latest.json`. Do not move the old v0.1.0 Windows agent alias; existing installers still depend on it. Use `-Type agent`, `-Type client` or `-Type gateway` with the release script; the last two fail until their actual Cargo packages are implemented. The script refuses when tracked build inputs differ from `HEAD` unless `-AllowDirty` is passed, refuses to label a host build as another target, refuses to add a target from a different commit, and requires `-Replace` to rebuild an existing target from a different commit.

On Linux/macOS, where a matching release binary exists, use `sh ./install.sh --type agent` or install from source. The local Unix installer uses Python 3 only for checksum-verified installation; no Python runtime is bundled into the Rust agent. Current repository publishes no Linux/macOS binaries.

## Self-distributed updates

Installed agents can pull the same typed channel themselves, either on demand or continuously:

```powershell
jani update check --source https://releases.example.com/
jani update install --source https://releases.example.com/
jani update install --source https://releases.example.com/ --to C:\path\to\coding-agent\jani.exe
jani update watch --source https://releases.example.com/ --interval 300
```

`check` reports the channel version, each target's version and whether an update is available without downloading an artifact. `install` downloads the artifact once, verifies its exact size and SHA-256 against the typed manifest, runs the candidate's `--version` before replacing anything, replaces each target and writes a receipt under `<state-dir>/updates/`. The default target is the running executable; repeat `--to` to push the same verified artifact to additional local binaries such as a coding-agent copy. Windows cannot overwrite a running image, so the updater renames the old image aside, reports `restartRequired` when it replaced its own executable, and keeps a `.jani-old-<id>` backup if the old image stays locked. `watch` polls the channel at the interval and applies each newer release as it appears; `--cycles` bounds the loop, zero means forever.

The origin must be HTTPS, or loopback HTTP for local development. Redirects are refused, metadata and artifacts are size-bounded, and a digest or size mismatch is rejected before any target is touched. Manifests are not cryptographically signed: delivery integrity rests on the HTTPS origin and the manifest digest, so control of the origin is control of updates.

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
- There is no managed identity, signing certificate or rollback service behind this installer. `jani update` verifies HTTPS delivery, manifest digest and candidate execution, but manifests are not signed and a replaced binary is not rolled back automatically. Installation remains a verified local file copy.
- Building from source does not supply trusted policy roots or enterprise isolation.

## Acceptance

- A valid artifact installs and reports its version; a tampered artifact is refused before any file is copied.
- A fresh install still denies writes and command execution until explicit scopes are supplied.

## Maintenance

Review exact source and tests before asserting support for a platform or operation. Missing evidence blocks readiness.
