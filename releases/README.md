# Jani typed release catalogue (v2)

**Canonical layout:** `releases/{type}/{platform}/{version}/`, with immutable component and platform metadata. Version directories use tags such as `v0.1.0`; architecture/ABI is recorded by Rust target triple inside each manifest.

```text
releases/
  agent/
    windows/
      latest.json
      v0.1.0/
        manifest.json
        jani-0.1.0-x86_64-pc-windows-msvc.exe
        jani-0.1.0-x86_64-pc-windows-msvc.exe.sha256
    macos/                 # appears only when an artifact exists
    linux/                 # appears only when an artifact exists
  client/
    windows/ linux/ macos/ # only after a real jani-probe binary is built
  gateway/
    windows/ linux/ macos/ # only after a real jani-gateway binary is built
  latest.json             # frozen legacy pointer for old installers
  v0.1.0/                 # legacy Windows artifact alias, retained to avoid breakage
```

Component identifiers: `agent` (`jani`), `client` (`jani-probe`) and `gateway` (`jani-gateway`). Platform identifiers: `windows`, `macos`, `linux`. These are not interchangeable with Rust target triples: `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` both belong to `linux` but require different binaries. **A directory name does not prove a binary exists.** Only a fully verified manifest and checksum designate a release.

## Release metadata

`{type}/{platform}/latest.json` has `jani.release-channel/v2`, exact `type`, exact `platform`, and `latest` version. Versioned `manifest.json` uses `jani.release/v2`, `type`, `platform`, `package`, `version`, `tag`, `commit`, `dirty`, `builtAt`, and an artifact array. Each artifact binds target triple, filename, SHA-256, size, source commit, dirty flag and build time. A release cannot silently mix commits across targets.

The typed Windows agent release at [agent/windows/v0.1.0](agent/windows/v0.1.0/manifest.json) was repackaged from commit `a3f7b03` on 2026-10-26, with clean build inputs and its own SHA-256. The old `releases/v0.1.0/` remains a compatibility alias for existing installers and still holds the earlier `c9e3f3b` build; do not create new legacy releases. No client or gateway artifacts are currently published.

## Installer

On Windows run `powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1` from repository root. `-Type agent` (default), `-Type client` or `-Type gateway` selects a component, while `-Version` and `-Target` choose a version and compatible target. Installer chooses typed channels, validates manifest type/platform/target and SHA-256, and **only for a legacy Windows agent** may fall back to the deprecated version directory. It installs nothing when client/gateway binaries do not exist.

There is no auto-update channel, OS signing, security attestation, or managed certificate provisioning behind these local installer scripts. Platform support and threat containment must be validated separately.

## Release publisher

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\release.ps1 -Type agent
# Future, only after the packages exist in Cargo:
# .\tools\release.ps1 -Type client
# .\tools\release.ps1 -Type gateway
```

The builder checks the actual installed Rust host target, `cargo metadata` package existence, clean tracked build inputs, SHA-256 and existing target conflicts. It refuses to fake platform builds and refuses to replace an existing target without `-Replace`. It writes the typed version directory and typed platform-specific `latest.json` (not the frozen v1 global pointer). Commit those files intentionally only after verification. It does not claim release-signature provenance.

## CI verification

```bash
python tools/release_catalog.py check
python -m unittest discover -s tests -p 'test_release_catalog.py' -v
```

The catalog verifies canonical manifests, target/platform identity, exact artifact bytes against SHA-256, sidecars, and current channels. The v1 global pointer is checked for legacy availability. Tests include tampered binaries and mislabelled builds. An all-green catalogue is **integrity evidence, not production security certification**. See [Sentinel gates](../docs/SENTINEL_RELEASE_GATES.md) and [release acceptance](../docs/RELEASE_ACCEPTANCE.md).
