[CmdletBinding()]
param(
    [string]$Version,
    [string]$Target,
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
    $package = @($metadata.packages) | Where-Object { $_.name -eq 'jani' } | Select-Object -First 1
    if (-not $package) {
        Fail 'cannot find the jani package in cargo metadata'
    }
    $Version = [string]$package.version
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

$sourcePaths = @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'backend', 'contract')
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

& cargo build --release -p jani --locked
if ($LASTEXITCODE -ne 0) {
    Fail 'cargo build failed'
}

$exeName = if ($Target -like '*windows*') { 'jani.exe' } else { 'jani' }
$builtPath = Join-Path (Join-Path ([string]$metadata.target_directory) 'release') $exeName
if (-not (Test-Path -LiteralPath $builtPath)) {
    Fail "missing built binary: $builtPath"
}

$tag = "v$Version"
$releaseDir = Join-Path (Join-Path $root 'releases') $tag
New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null

$extension = if ($Target -like '*windows*') { '.exe' } else { '' }
$binaryName = "jani-$Version-$Target$extension"
$binaryPath = Join-Path $releaseDir $binaryName
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
    if ($existing.schemaVersion -ne 'jani.release/v1') {
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
    schemaVersion = 'jani.release/v1'
    version = $Version
    tag = $tag
    commit = $commit
    dirty = $dirty
    builtAt = $builtAt
    artifacts = @($artifacts)
}
[IO.File]::WriteAllText($manifestPath, (($manifest | ConvertTo-Json -Depth 6) + "`n"), $utf8)

$latest = [ordered]@{
    schemaVersion = 'jani.release-channel/v1'
    latest = $tag
}
[IO.File]::WriteAllText((Join-Path (Join-Path $root 'releases') 'latest.json'), (($latest | ConvertTo-Json) + "`n"), $utf8)

Write-Host "Release $tag built from $commit (build inputs dirty: $dirty)"
Write-Host "Binary:   $binaryPath"
Write-Host "SHA-256:  $hash"
Write-Host "Manifest: $manifestPath"
Write-Host "Channel:  releases\latest.json"
Write-Host 'Commit the version directory and latest.json to make this release available to installers.'
