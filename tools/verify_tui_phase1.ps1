# Phase 1A verification for Jani on a disposable Windows checkout.
# No publication, installation or elevation is performed.
param(
    [string]$Binary = "target/release/jani.exe",
    [string]$Scratch = "target/tui-phase1-probe"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Invoke-Checked {
    param(
        [string]$Program,
        [string[]]$Arguments
    )
    Write-Host ("[jani verify] " + $Program + " " + ($Arguments -join " "))
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program exited with status $LASTEXITCODE"
    }
}

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Push-Location $root
try {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "Cargo is required. Install the pinned Rust toolchain before verification."
    }
    if (-not (Get-Command python -ErrorAction SilentlyContinue)) {
        throw "Python is required for doccheck and the Windows console probe."
    }
    if ($PSVersionTable.PSEdition -eq "Core" -and -not $IsWindows) {
        throw "This verification script requires Windows for its terminal console probe."
    }
    # PowerShell 5.1 has no IsWindows variable; in that host, OS is Windows.
    Invoke-Checked -Program "cargo" -Arguments @("fmt", "--all", "--", "--check")
    Invoke-Checked -Program "cargo" -Arguments @("clippy", "--workspace", "--all-targets", "--", "-D", "warnings")
    Invoke-Checked -Program "cargo" -Arguments @("test", "--workspace", "--all-targets")
    Invoke-Checked -Program "cargo" -Arguments @("run", "-p", "jani-check")
    Invoke-Checked -Program "cargo" -Arguments @("run", "-p", "jani-claims", "--", "check")
    Invoke-Checked -Program "python" -Arguments @("tools/doccheck.py")
    Invoke-Checked -Program "cargo" -Arguments @("build", "--locked", "--release", "-p", "jani")
    if (-not (Test-Path $Binary -PathType Leaf)) {
        throw "Built Jani binary not found: $Binary"
    }
    Invoke-Checked -Program "python" -Arguments @("tools/tui_console_probe.py", $Binary, $Scratch)

    $hash = (Get-FileHash -Path $Binary -Algorithm SHA256).Hash
    $commit = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "Cannot determine Git commit SHA"
    }
    Write-Host ("[jani verify] PASS commit=" + $commit + " binary_sha256=" + $hash)
    Write-Host "[jani verify] This proves the local gates only, not a production security release."
}
finally {
    Pop-Location
}
