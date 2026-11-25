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

`{type}/{platform}/latest.json` has `jani.release-channel/v2`, exact `type`, exact `platform`, and `latest` version. Versioned `manifest.json` uses `jani.release/v2`, `type`, `platform`, `package`, `version`, `tag`, `commit`, `dirty`, `builtAt`, an artifact array, and an Ed25519 `signature` object. Each artifact binds target triple, filename, SHA-256, size, source commit, dirty flag and build time. A release cannot silently mix commits across targets. The signature is over `b"jani-release-manifest/v2\0"` followed by compact, sorted-key UTF-8 JSON of the manifest with `signature` removed; its object contains only `keyId` and the 128-hex-character `value`.

The typed Windows agent release at [agent/windows/v0.1.0](agent/windows/v0.1.0/manifest.json) was repackaged from commit `a4bc254` on 2026-10-26, with clean build inputs and its own SHA-256. The old `releases/v0.1.0/` remains a compatibility alias for existing installers and still holds the earlier `c9e3f3b` build; do not create new legacy releases. No client or gateway artifacts are currently published. The committed v0.1.x typed manifests predate signing and are refused by the hardened updater and typed installers until a publisher re-signs them.

## Installer

On Windows run `powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -UpdateKey release-2026:<64_HEX_PUBLIC_KEY>` from repository root. `-Type agent` (default), `-Type client` or `-Type gateway` selects a component, while `-Version` and `-Target` choose a version and compatible target. The installer chooses typed channels, verifies the Ed25519-signed manifest against every explicitly trusted `-UpdateKey`, validates manifest type/platform/target and exact SHA-256/size, then runs a temporary candidate with provider credentials removed before copying it to the install directory. Repeat `-UpdateKey` for additional trusted signers. Typed installation requires Python 3 and OpenSSL 3 with Ed25519 support; it never accepts an unsigned v2 manifest.

Only the frozen legacy Windows agent v1 directory may be selected as a fallback, and only with explicit `-AllowUnsignedLegacy`. That switch never weakens typed v2 verification. The installer installs nothing when client/gateway binaries do not exist. On Linux/macOS, use `sh ./install.sh --type agent --update-key release-2026:<64_HEX_PUBLIC_KEY>` when a signed matching artifact exists; the source-checkout Unix installer has the same Python 3/OpenSSL 3 requirement.

There is no Authenticode code signing, security attestation, or managed certificate provisioning behind these local installer scripts. Windows SmartScreen may therefore warn even though the release manifest signature and artifact digest were verified. Platform support and threat containment must be validated separately.

## Release publisher

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\release.ps1 `
  -Type agent -UpdateKeyPath C:\Keys\jani-release-private.pem -UpdateKeyId release-2026
# Future, only after the packages exist in Cargo:
# .\tools\release.ps1 -Type client -UpdateKeyPath C:\Keys\jani-release-private.pem -UpdateKeyId release-2026
# .\tools\release.ps1 -Type gateway -UpdateKeyPath C:\Keys\jani-release-private.pem -UpdateKeyId release-2026
```

The builder checks the actual installed Rust host target, `cargo metadata` package existence, clean tracked build inputs, SHA-256 and existing target conflicts. It refuses to fake platform builds and refuses to replace an existing target without `-Replace`. Python 3 and OpenSSL 3 with Ed25519 support are required. The private key must remain outside the repository; the script refuses an in-tree key, signs the versioned manifest before advancing the typed platform-specific `latest.json`, and prints only the public `ID:HEX` trust root. It never updates the frozen v1 global pointer. Commit the version directory and typed channel pointer intentionally only after verification; signature verification still depends on the operator explicitly trusting the printed public key.

## CI verification

```bash
python tools/release_catalog.py check
python tools/release_catalog.py check --require-signatures --update-key release-2026:<64_HEX_PUBLIC_KEY>
python -m unittest discover -s tests -p 'test_release_catalog.py' -v
```

The catalog verifies canonical manifests, target/platform identity, exact artifact bytes against SHA-256, sidecars, and current channels. The v1 global pointer is checked for legacy availability. Default catalog checks preserve byte-integrity coverage for the committed unsigned v0.1.x history; `--require-signatures` refuses every unsigned v2 manifest, and signed manifests verify only with explicitly supplied `--update-key` roots. The current catalogue therefore fails strict signature mode until the publisher re-signs a release.

Tests use a temporary dummy Ed25519 key and cover valid signatures, missing/wrong trust, unsigned strict-mode refusal, manifest tampering, and signed Unix-installer verification. OpenSSL 3 with Ed25519 support is required for those signing tests. An all-green catalogue is **integrity evidence, not production security certification**. See [Sentinel gates](../docs/SENTINEL_RELEASE_GATES.md) and [release acceptance](../docs/RELEASE_ACCEPTANCE.md).
