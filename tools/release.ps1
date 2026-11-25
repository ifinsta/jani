[CmdletBinding()]
param(
    [string]$Version,
    [string]$Target,
    [string]$Type = 'agent',
    [string]$UpdateKeyPath,
    [string]$UpdateKeyId,
    [switch]$AllowDirty,
    [switch]$Replace
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Fail([string]$Message) {
    throw "release: $Message"
}

$contractPath = Join-Path $root 'contract\releases.json'
$contract = Get-Content -Raw -LiteralPath $contractPath | ConvertFrom-Json
if ($contract.schemaVersion -ne 'jani.release-contract/v1') {
    Fail "unsupported release contract: $contractPath"
}
if ($Type -notin @($contract.types.PSObject.Properties.Name)) {
    Fail "unsupported release type '$Type'"
}
$packageName = [string]$contract.types.$Type
if (-not $UpdateKeyPath -or -not $UpdateKeyId) {
    Fail 'release publishing requires -UpdateKeyPath and -UpdateKeyId; private keys stay outside the repository'
}
if ($UpdateKeyId.Trim() -ne $UpdateKeyId -or -not $UpdateKeyId) {
    Fail 'release key id must be nonempty and have no surrounding whitespace'
}
if (-not (Test-Path -LiteralPath $UpdateKeyPath -PathType Leaf)) {
    Fail "release private key not found: $UpdateKeyPath"
}
if (-not (Get-Command python -ErrorAction SilentlyContinue)) {
    Fail 'python 3 is required for release signing'
}
if (-not (Get-Command openssl -ErrorAction SilentlyContinue)) {
    Fail 'OpenSSL 3 with Ed25519 support is required for release signing'
}
$UpdateKeyPath = [IO.Path]::GetFullPath($UpdateKeyPath)
$rootFull = [IO.Path]::GetFullPath($root)
$rootPrefix = $rootFull.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if ($UpdateKeyPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    Fail 'release private key must stay outside the repository'
}

function Get-HostTarget {
    $rustc = Get-Command rustc -ErrorAction SilentlyContinue
    if ($rustc) {
        $line = & rustc -vV 2>$null | Select-String '^host:\s*'
        if ($line) {
            return ($line.ToString() -replace '^host:\s*', '').Trim()
        }
    }
    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        if ([Environment]::Is64BitOperatingSystem) {
            return [string]$contract.targets.'windows-x86_64'
        }
        return 'i686-pc-windows-msvc'
    }
    Fail 'cannot determine the host target; pass -Target (for example x86_64-unknown-linux-gnu)'
}

function Get-Platform([string]$TargetName) {
    foreach ($property in $contract.platforms.PSObject.Properties) {
        if ($TargetName -match $property.Value) { return $property.Name }
    }
    Fail "unsupported release platform '$TargetName'"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Fail 'cargo is not on PATH'
}
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Fail 'git is not on PATH'
}

$metadataJson = & cargo metadata --format-version 1 --no-deps
if ($LASTEXITCODE -ne 0) {
    Fail 'cargo metadata failed'
}
$metadata = $metadataJson | ConvertFrom-Json

if (-not $Version) {
    $package = @($metadata.packages) | Where-Object { $_.name -eq $packageName } | Select-Object -First 1
    if (-not $package) {
        Fail "package '$packageName' is not implemented in this workspace; cannot publish a fictional $Type binary"
    }
    $Version = [string]$package.version
}

if (-not (@($metadata.packages) | Where-Object { $_.name -eq $packageName })) {
    Fail "package '$packageName' is not implemented in this workspace; cannot publish a fictional $Type binary"
}

$Version = $Version -replace '^[vV]', ''
if ($Version -notmatch '^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]+)?$') {
    Fail "invalid semantic version '$Version'"
}
$hostTarget = Get-HostTarget
if (-not $Target) {
    $Target = $hostTarget
} elseif ($Target -ne $hostTarget) {
    Fail "cross-compilation is not supported by this script; run it on a '$Target' host"
}

$sourcePaths = @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'backend', 'contract', 'protocol')
if (Test-Path -LiteralPath '.cargo') {
    $sourcePaths += '.cargo'
}
$status = @(git status --porcelain -- $sourcePaths)
$dirty = $status.Count -gt 0
if ($dirty -and -not $AllowDirty) {
    $details = $status -join "`n"
    Fail "tracked build inputs differ from HEAD:`n$details`nCommit them first, or pass -AllowDirty for a local diagnostic build."
}
$commit = (& git rev-parse HEAD 2>$null)
if ($LASTEXITCODE -ne 0 -or -not $commit) {
    Fail 'cannot resolve HEAD; run this from a Git checkout'
}
$commit = $commit.Trim()

& cargo build --release -p $packageName --locked
if ($LASTEXITCODE -ne 0) {
    Fail 'cargo build failed'
}

$exeName = if ($Target -like '*windows*') { "$packageName.exe" } else { $packageName }
$builtPath = Join-Path (Join-Path ([string]$metadata.target_directory) 'release') $exeName
if (-not (Test-Path -LiteralPath $builtPath)) {
    Fail "missing built binary: $builtPath"
}

$tag = "v$Version"
$platform = Get-Platform $Target
$releaseDir = Join-Path (Join-Path (Join-Path (Join-Path $root 'releases') $Type) $platform) $tag
New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null

$extension = if ($Target -like '*windows*') { '.exe' } else { '' }
$binaryName = "$packageName-$Version-$Target$extension"
$binaryPath = Join-Path $releaseDir $binaryName
$manifestPath = Join-Path $releaseDir 'manifest.json'
# Check immutable version/commit/target rules before overwriting published binary.
if (Test-Path -LiteralPath $manifestPath) {
    $previous = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if ($previous.schemaVersion -ne $contract.schemas.manifest -or $previous.version -ne $Version -or
        $previous.type -ne $Type -or $previous.platform -ne $platform) {
        Fail 'existing release manifest conflicts with requested component/platform/version'
    }
    $sameTarget = @(@($previous.artifacts) | Where-Object { $_.target -eq $Target }) | Select-Object -First 1
    if ([string]$previous.commit -ne $commit) {
        if (-not $sameTarget -or -not $Replace -or @($previous.artifacts).Count -gt 1) {
            Fail 'release version already belongs to another source commit; bump the version'
        }
    } elseif ($sameTarget -and -not $Replace) {
        Fail 'existing target cannot be overwritten without -Replace'
    }
}
Copy-Item -LiteralPath $builtPath -Destination $binaryPath -Force

$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $binaryPath).Hash.ToLowerInvariant()
$size = (Get-Item -LiteralPath $binaryPath).Length

$utf8 = New-Object System.Text.UTF8Encoding($false)
$checksumName = "$binaryName.sha256"
[IO.File]::WriteAllText((Join-Path $releaseDir $checksumName), "$hash  $binaryName`n", $utf8)

$manifestPath = Join-Path $releaseDir 'manifest.json'
$artifacts = @()
$builtAt = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')

if (Test-Path -LiteralPath $manifestPath) {
    $existing = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if ($existing.schemaVersion -ne $contract.schemas.manifest -or $existing.type -ne $Type -or $existing.platform -ne $platform) {
        Fail "unsupported existing manifest schema in $manifestPath"
    }
    if ([string]$existing.version -ne $Version) {
        Fail "existing manifest version '$($existing.version)' does not match '$Version'"
    }
    $existingArtifact = @(@($existing.artifacts) | Where-Object { $_.target -eq $Target }) | Select-Object -First 1
    if ([string]$existing.commit -ne $commit) {
        if (-not $existingArtifact) {
            Fail "release $tag records commit $($existing.commit); bump the version before adding another target"
        }
        if (-not $Replace) {
            Fail "release $tag already records commit $($existing.commit); pass -Replace to rebuild this target from $commit, or bump the version"
        }
    }
    $artifacts = @(@($existing.artifacts) | Where-Object { $_.target -ne $Target })
}

$artifacts += [ordered]@{
    target = $Target
    binary = $binaryName
    sha256 = $hash
    sizeBytes = $size
    commit = $commit
    dirty = $dirty
    builtAt = $builtAt
}

$manifest = [ordered]@{
    schemaVersion = $contract.schemas.manifest
    type = $Type
    platform = $platform
    package = $packageName
    version = $Version
    tag = $tag
    commit = $commit
    dirty = $dirty
    builtAt = $builtAt
    artifacts = @($artifacts)
}
$manifestTmp = "$manifestPath.tmp.$([guid]::NewGuid().ToString('N'))"
[IO.File]::WriteAllText($manifestTmp, (($manifest | ConvertTo-Json -Depth 6) + "`n"), $utf8)
Move-Item -LiteralPath $manifestTmp -Destination $manifestPath -Force

$signingTool = Join-Path $PSScriptRoot 'release_signing.py'
& python $signingTool sign --manifest $manifestPath --key $UpdateKeyPath --key-id $UpdateKeyId
if ($LASTEXITCODE -ne 0) {
    Fail 'release manifest signing failed; channel pointer was not updated'
}
$publicKey = (& python $signingTool public-key --key $UpdateKeyPath)
if ($LASTEXITCODE -ne 0 -or -not $publicKey) {
    Fail 'cannot derive the release public key'
}
$publicKey = ([string]$publicKey).Trim()

$latest = [ordered]@{
    schemaVersion = $contract.schemas.channel
    type = $Type
    platform = $platform
    latest = $tag
}
$channelPath = Join-Path (Join-Path (Join-Path $root 'releases') $Type) $platform
$latestTarget = Join-Path $channelPath 'latest.json'
$latestTmp = "$latestTarget.tmp.$([guid]::NewGuid().ToString('N'))"
[IO.File]::WriteAllText($latestTmp, (($latest | ConvertTo-Json) + "`n"), $utf8)
Move-Item -LiteralPath $latestTmp -Destination $latestTarget -Force
# Legacy channel is frozen as a compatibility pointer for existing v0.1.0 installers.

Write-Host "Release $Type/$platform/$tag built from $commit (build inputs dirty: $dirty)"
Write-Host "Binary:   $binaryPath"
Write-Host "SHA-256:  $hash"
Write-Host "Manifest: $manifestPath"
Write-Host "Trust key: ${UpdateKeyId}:$publicKey"
Write-Host "Channel:  releases\$Type\$platform\latest.json"
Write-Host 'Commit the typed platform/version folder and its typed latest.json to publish the artifact.'
