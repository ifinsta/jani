[CmdletBinding()]
param(
    [string]$Version,
    [string]$Target,
    [string]$Type = 'agent',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Jani\bin'),
    [switch]$AddToPath,
    [switch]$NoPath
)

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$releases = Join-Path $root 'releases'
$InstallDir = [IO.Path]::GetFullPath($InstallDir)

function Fail([string]$Message) {
    throw "install: $Message"
}

$contractPath = Join-Path $root 'contract\releases.json'
$contract = Get-Content -Raw -LiteralPath $contractPath | ConvertFrom-Json
if ($contract.schemaVersion -ne 'jani.release-contract/v1') {
    Fail "unsupported release contract: $contractPath"
}
if ($Type -notin @($contract.types.PSObject.Properties.Name)) {
    Fail "unsupported release type '$Type'"
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

if ($AddToPath -and $NoPath) {
    Fail '-AddToPath and -NoPath cannot be used together'
}

function Add-UserPathEntry {
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @((($current + '') -split ';') | Where-Object { $_ })
    if (-not ($entries -contains $InstallDir)) {
        try {
            [Environment]::SetEnvironmentVariable('Path', ((@($entries) + $InstallDir) -join ';'), 'User')
            Write-Host "Added $InstallDir to the user PATH. Restart your terminal to pick it up."
        } catch {
            Write-Warning "Could not update the user PATH: $_"
        }
    }
    $env:Path = "$InstallDir;$env:Path"
}

if (-not $Target) {
    $Target = Get-HostTarget
}
$platform = Get-Platform $Target
$channelDir = Join-Path (Join-Path $releases $Type) $platform
$channelPath = Join-Path $channelDir 'latest.json'
$legacy = $false

if (-not $Version) {
    if (Test-Path -LiteralPath $channelPath) {
        $channel = Get-Content -Raw -LiteralPath $channelPath | ConvertFrom-Json
        if ($channel.schemaVersion -ne $contract.schemas.channel -or
            $channel.type -ne $Type -or $channel.platform -ne $platform -or
            [string]$channel.latest -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+') {
            Fail "invalid typed release channel: $channelPath"
        }
        $Version = [string]$channel.latest
    } elseif ($Type -eq 'agent' -and $platform -eq 'windows') {
        $latestPath = Join-Path $releases 'latest.json'
        if (-not (Test-Path -LiteralPath $latestPath)) {
            Fail 'no matching release channel; build the component from source'
        }
        $channel = Get-Content -Raw -LiteralPath $latestPath | ConvertFrom-Json
        if ($channel.schemaVersion -ne $contract.schemas.legacyChannel) {
            Fail 'unsupported legacy release channel'
        }
        $Version = [string]$channel.latest
    } else {
        Fail "no published $Type/$platform release channel; no binary was fabricated"
    }
}

$Version = $Version -replace '^[vV]', ''
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.+-]+)?$') {
    Fail "invalid release version '$Version'"
}
$tag = "v$Version"
$releaseDir = Join-Path $channelDir $tag
$manifestPath = Join-Path $releaseDir 'manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath)) {
    if ($Type -eq 'agent' -and $platform -eq 'windows') {
        $oldDir = Join-Path $releases $tag
        $oldManifestPath = Join-Path $oldDir 'manifest.json'
        if (Test-Path -LiteralPath $oldManifestPath) {
            $releaseDir = $oldDir
            $manifestPath = $oldManifestPath
            $legacy = $true
        }
    }
}
if (-not (Test-Path -LiteralPath $manifestPath)) {
    Fail "missing release manifest: $manifestPath"
}
$manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
if ($legacy) {
    if ($manifest.schemaVersion -ne $contract.schemas.legacyManifest) {
        Fail 'unsupported legacy release manifest'
    }
} elseif ($manifest.schemaVersion -ne $contract.schemas.manifest -or
          $manifest.type -ne $Type -or $manifest.platform -ne $platform) {
    Fail "release manifest does not match $Type/$platform"
}
if ([string]$manifest.version -ne $Version -or [string]$manifest.tag -ne $tag) {
    Fail "manifest identity does not match $tag"
}
$artifact = @($manifest.artifacts) | Where-Object { $_.target -eq $Target } | Select-Object -First 1
if (-not $artifact) {
    $available = (@($manifest.artifacts) | ForEach-Object { $_.target }) -join ', '
    Fail "release $Type/$platform/$tag has no artifact for '$Target'. Available: $available"
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

$binaryName = [string]$contract.types.$Type
$exeName = if ($Target -like '*windows*') { "$binaryName.exe" } else { $binaryName }
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$destination = Join-Path $InstallDir $exeName
Copy-Item -LiteralPath $binaryPath -Destination $destination -Force

$installed = & $destination --version 2>&1
if ($LASTEXITCODE -ne 0) {
    Fail "installed binary failed to run: $installed"
}

Write-Host "Installed $installed to $destination"
Write-Host "Verified SHA-256 $actual"
Write-Warning 'Jani release binaries are not code-signed; verify the checksum, manifest and source commit before use.'

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$pathEntries = @((($userPath + ';' + $machinePath) -split ';') | Where-Object { $_ })
$onPath = $pathEntries -contains $InstallDir

if ($AddToPath) {
    Add-UserPathEntry
} elseif ($onPath) {
    Write-Host "$InstallDir is already on PATH."
} else {
    $interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected
    $answer = ''
    if ($interactive -and -not $NoPath) {
        try {
            $answer = Read-Host "Add $InstallDir to your user PATH? [y/N]"
        } catch {
            $answer = ''
        }
    }
    if ($answer -match '^(y|yes)$') {
        Add-UserPathEntry
    } else {
        Write-Host "To use jani in this session: `$env:Path = `"$InstallDir;`$env:Path`""
        Write-Host "To add it permanently, rerun with -AddToPath"
    }
}
