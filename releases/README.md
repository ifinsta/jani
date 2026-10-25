# Jani releases

This directory is Jani's release channel. A release is a versioned folder containing a platform binary, its SHA-256 checksum and a machine-readable manifest. `latest.json` tells the installer which version to use by default.

## Install with one command

From the repository root on Windows x64:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

The installer:

1. Reads `releases/latest.json`.
2. Selects the artifact matching your host target.
3. Verifies the SHA-256 digest from the manifest.
4. Refuses to install on a missing binary, malformed manifest or checksum mismatch.
5. Copies the binary to `%LOCALAPPDATA%\Jani\bin` and runs `jani --version`.

No Rust toolchain is needed when a matching artifact exists. Options:

```powershell
# Install a specific version
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Version v0.1.0

# Install somewhere else
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -InstallDir C:\Tools\Jani\bin

# Add the install directory to your user PATH (persistent; opt-in)
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -AddToPath

# Override host detection (must match an artifact in the manifest)
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Target x86_64-pc-windows-msvc
```

If no matching artifact exists, install from source instead:

```powershell
cargo install --path backend/server --locked
```

## Layout

```text
releases/
  latest.json
  v0.1.0/
    manifest.json
    jani-0.1.0-x86_64-pc-windows-msvc.exe
    jani-0.1.0-x86_64-pc-windows-msvc.exe.sha256
```

`latest.json` is the channel pointer:

```json
{
  "schemaVersion": "jani.release-channel/v1",
  "latest": "v0.1.0"
}
```

Each `manifest.json` records the release version, source commit, build time, whether the tree was dirty, and one entry per target with the binary name, digest and size. The manifest is the authority the installer reads; `latest.json` only chooses the folder.

## Verify by hand

```powershell
Get-FileHash -Algorithm SHA256 .\releases\v0.1.0\jani-0.1.0-x86_64-pc-windows-msvc.exe
Get-Content .\releases\v0.1.0\jani-0.1.0-x86_64-pc-windows-msvc.exe.sha256
Get-Content .\releases\v0.1.0\manifest.json
```

All three digests must match. The checksum detects corruption or substitution of the committed artifact; it does not make an untrusted commit trustworthy. Review the source commit recorded in the manifest before running the binary.

## Publish a new release

1. Update the workspace version in `Cargo.toml` and add a `CHANGELOG.md` entry.
2. Commit your source changes so the release records a real source commit.
3. From the repository root, run:

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\release.ps1
   ```

   The script builds `cargo build --release -p jani --locked`, writes `releases/v<version>/`, refreshes `releases/latest.json` and prints the digest. It refuses a dirty tree unless you pass `-AllowDirty`; bootstrap and diagnostic builds may use that flag, and the manifest records `"dirty": true`.

4. Commit the new version folder and the updated `latest.json`. Those committed files are what the installer distributes.

For additional platforms, run the script on each target host. It deliberately refuses to label a host build as a different target.

## Trust and limits

- **Binaries are committed intentionally.** Every platform release adds roughly the binary size to Git history. If that becomes a problem, keep this manifest format and move artifacts to GitHub Releases, updating the installer to download and verify them.
- **No code signing.** Jani release binaries are unsigned, so Windows SmartScreen may warn. Verify the checksum and the manifest's source commit, or build from source.
- **No credential or authority setup.** Installing the binary does not configure API keys, write scopes, signed policy or sandbox support. Those remain explicit runtime decisions.
- **Platform-specific.** A committed artifact only runs on its recorded target. Do not assume Windows isolation applies to Linux or macOS; see [VALIDATION.md](../VALIDATION.md) and [docs/PLATFORM_SUPPORT.md](../docs/PLATFORM_SUPPORT.md).
- **Not a release certification.** A folder here proves a binary was built, not that release acceptance evidence exists. See [docs/RELEASE_ACCEPTANCE.md](../docs/RELEASE_ACCEPTANCE.md).
