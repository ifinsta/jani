[CmdletBinding()]
param(
    [string]$Version,
    [string]$Target,
    [ValidateSet('agent', 'client', 'gateway')]
    [string]$Type = 'agent',
    [switch]$AllowDirty,
    [switch]$Replace
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Fail([string]$Message) {
    throw "release: $Message"
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
            return 'x86_64-pc-windows-msvc'
        }
        return 'i686-pc-windows-msvc'
    }
    Fail 'cannot determine the host target; pass -Target (for example x86_64-unknown-linux-gnu)'
}

$packages = @{ agent = 'jani'; client = 'jani-probe'; gateway = 'jani-gateway' }
$binaryNames = @{ agent = 'jani'; client = 'jani-probe'; gateway = 'jani-gateway' }
$packageName = $packages[$Type]

function Get-Platform([string]$TargetName) {
    if ($TargetName -match 'windows') { return 'windows' }
    if ($TargetName -match '(apple-darwin|darwin)') { return 'macos' }
    if ($TargetName -match 'linux') { return 'linux' }
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

$exeName = if ($Target -like '*windows*') { "$($binaryNames[$Type]).exe" } else { [string]$binaryNames[$Type] }
$builtPath = Join-Path (Join-Path ([string]$metadata.target_directory) 'release') $exeName
if (-not (Test-Path -LiteralPath $builtPath)) {
    Fail "missing built binary: $builtPath"
}

$tag = "v$Version"
$platform = Get-Platform $Target
$releaseDir = Join-Path (Join-Path (Join-Path (Join-Path $root 'releases') $Type) $platform) $tag
New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null

$extension = if ($Target -like '*windows*') { '.exe' } else { '' }
$binaryName = "$($binaryNames[$Type])-$Version-$Target$extension"
$binaryPath = Join-Path $releaseDir $binaryName
$manifestPath = Join-Path $releaseDir 'manifest.json'
# Check immutable version/commit/target rules before overwriting published binary.
if (Test-Path -LiteralPath $manifestPath) {
    $previous = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if ($previous.schemaVersion -ne 'jani.release/v2' -or $previous.version -ne $Version -or
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
    if ($existing.schemaVersion -ne 'jani.release/v2' -or $existing.type -ne $Type -or $existing.platform -ne $platform) {
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
    schemaVersion = 'jani.release/v2'
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
[IO.File]::WriteAllText($manifestPath, (($manifest | ConvertTo-Json -Depth 6) + "`n"), $utf8)

$latest = [ordered]@{
    schemaVersion = 'jani.release-channel/v2'
    type = $Type
    platform = $platform
    latest = $tag
}
$channelPath = Join-Path (Join-Path (Join-Path $root 'releases') $Type) $platform
[IO.File]::WriteAllText((Join-Path $channelPath 'latest.json'), (($latest | ConvertTo-Json) + "`n"), $utf8)
# Legacy channel is frozen as a compatibility pointer for existing v0.1.0 installers.

Write-Host "Release $Type/$platform/$tag built from $commit (build inputs dirty: $dirty)"
Write-Host "Binary:   $binaryPath"
Write-Host "SHA-256:  $hash"
Write-Host "Manifest: $manifestPath"
Write-Host "Channel:  releases\$Type\$platform\latest.json"
Write-Host 'Commit the typed platform/version folder and its typed latest.json to publish the artifact.'
