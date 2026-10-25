[CmdletBinding()]
param(
    [string]$Version,
    [string]$Target,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Jani\bin'),
    [switch]$AddToPath
)

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$releases = Join-Path $root 'releases'
$InstallDir = [IO.Path]::GetFullPath($InstallDir)

function Fail([string]$Message) {
    throw "install: $Message"
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

if (-not $Version) {
    $latestPath = Join-Path $releases 'latest.json'
    if (-not (Test-Path -LiteralPath $latestPath)) {
        Fail 'no release found. Run tools\release.ps1 first, or install from source with: cargo install --path backend/server --locked'
    }
    $latest = Get-Content -Raw -LiteralPath $latestPath | ConvertFrom-Json
    if (-not $latest.latest) {
        Fail "invalid release channel: $latestPath"
    }
    $Version = [string]$latest.latest
}

$Version = $Version -replace '^[vV]', ''
$tag = "v$Version"
$releaseDir = Join-Path $releases $tag
$manifestPath = Join-Path $releaseDir 'manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath)) {
    Fail "missing release manifest: $manifestPath"
}

$manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
if ($manifest.schemaVersion -ne 'jani.release/v1') {
    Fail "unsupported release manifest schema in $manifestPath"
}
if ([string]$manifest.version -ne $Version) {
    Fail "manifest version '$($manifest.version)' does not match requested version '$Version'"
}

if (-not $Target) {
    $Target = Get-HostTarget
}

$artifact = @($manifest.artifacts) | Where-Object { $_.target -eq $Target } | Select-Object -First 1
if (-not $artifact) {
    $available = (@($manifest.artifacts) | ForEach-Object { $_.target }) -join ', '
    Fail "release $tag has no artifact for '$Target'. Available: $available"
}
if ($artifact.binary -match '[\\/]' -or $artifact.binary -match '\.\.') {
    Fail 'unexpected path in release manifest'
}
if ([string]$artifact.sha256 -notmatch '^[0-9a-fA-F]{64}$') {
    Fail 'release manifest has an invalid SHA-256 digest'
}

$binaryPath = Join-Path $releaseDir ([string]$artifact.binary)
if (-not (Test-Path -LiteralPath $binaryPath)) {
    Fail "missing release binary: $binaryPath"
}

$actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $binaryPath).Hash.ToLowerInvariant()
if ($actual -ne ([string]$artifact.sha256).ToLowerInvariant()) {
    Fail "checksum mismatch for $binaryPath; refusing to install"
}

$exeName = if ($Target -like '*windows*') { 'jani.exe' } else { 'jani' }
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$destination = Join-Path $InstallDir $exeName
Copy-Item -LiteralPath $binaryPath -Destination $destination -Force

$installed = & $destination --version 2>&1
if ($LASTEXITCODE -ne 0) {
    Fail "installed binary failed to run: $installed"
}

Write-Host "Installed $installed to $destination"
Write-Host "Verified SHA-256 $actual"
Write-Warning 'Jani release binaries are not code-signed; verify the checksum and source commit before use.'

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$pathEntries = @((($userPath + ';' + $machinePath) -split ';') | Where-Object { $_ })
$onPath = $pathEntries -contains $InstallDir

if ($AddToPath) {
    $userEntries = @((($userPath + '') -split ';') | Where-Object { $_ })
    if (-not ($userEntries -contains $InstallDir)) {
        [Environment]::SetEnvironmentVariable('Path', ((@($userEntries) + $InstallDir) -join ';'), 'User')
        Write-Host "Added $InstallDir to the user PATH. Restart your terminal to pick it up."
    }
    $env:Path = "$InstallDir;$env:Path"
} elseif (-not $onPath) {
    Write-Host "To use jani in this session: `$env:Path = `"$InstallDir;`$env:Path`""
    Write-Host "To add it permanently, rerun with -AddToPath"
}
