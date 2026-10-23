<#
.SYNOPSIS
    TaskVault One-Click Environment Check & Setup Script (Windows 10/11 x64)

.DESCRIPTION
    Checks and optionally installs all dependencies required to develop and build TaskVault:
    1. Microsoft Edge WebView2 Runtime (Tauri 2 rendering engine)
    2. Visual Studio C++ Build Tools (MSVC linker & Windows SDK)
    3. Rust Toolchain (rustup + stable-x86_64-pc-windows-msvc + clippy + rustfmt, rustc >= 1.77.2)
    4. Tauri CLI (cargo-tauri ^2.0.0)
    5. Node.js Runtime (>= 18.0.0, for frontend unit tests and standalone preview server)

.PARAMETER CheckOnly
    Run environment diagnostics only without installing missing components.

.PARAMETER Yes
    Unattended mode: automatically install missing dependencies without prompting.
#>
param(
    [switch]$CheckOnly,
    [switch]$Yes
)

$ErrorActionPreference = "Stop"

function Write-Header([string]$Text) {
    Write-Host ""
    Write-Host "============================================================" -ForegroundColor Cyan
    Write-Host "  $Text" -ForegroundColor Cyan
    Write-Host "============================================================" -ForegroundColor Cyan
}

function Refresh-SessionPath {
    $machinePath = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    $wingetLinks = Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Links"
    $env:Path = "$cargoBin;$wingetLinks;$userPath;$machinePath"
}

function Test-CommandExists([string]$Name) {
    return $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Resolve-CargoTool([string]$Name) {
    # Wait up to 5 seconds in case rustup self-update is replacing binaries on Windows
    for ($i = 0; $i -lt 10; $i++) {
        Refresh-SessionPath
        $cargoCandidate = Join-Path $env:USERPROFILE ".cargo\bin\$Name.exe"
        if (Test-Path $cargoCandidate) {
            return $cargoCandidate
        }
        $cmd = Get-Command $Name -ErrorAction SilentlyContinue
        if ($null -ne $cmd) {
            return $cmd.Source
        }
        Start-Sleep -Milliseconds 500
    }
    return $null
}

function Test-WebView2Installed {
    $regPaths = @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        "HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    )
    foreach ($p in $regPaths) {
        if (Test-Path $p) {
            $pv = (Get-ItemProperty -Path $p -ErrorAction SilentlyContinue).pv
            if ($pv -and $pv -ne "0.0.0.0") {
                return $pv
            }
        }
    }
    return $null
}

function Test-MsvcInstalled {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $installPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
        if ($installPath) {
            return "Installed"
        }
    }
    if (Test-CommandExists "cl.exe") {
        return "Installed (PATH)"
    }
    return $null
}

function Get-RustcVersion {
    $rustcCmd = Resolve-CargoTool "rustc"
    if (-not $rustcCmd) { return $null }
    $out = (& $rustcCmd --version 2>$null) -join ""
    if ($out -match "rustc\s+(\d+\.\d+\.\d+)") {
        return $Matches[1]
    }
    return $null
}

function Test-TauriCliInstalled {
    $cargoCmd = Resolve-CargoTool "cargo"
    if (-not $cargoCmd) { return $null }
    $out = (& $cargoCmd tauri --version 2>$null) -join ""
    if ($LASTEXITCODE -eq 0 -and $out) {
        return $out.Trim()
    }
    return $null
}

function Get-NodeVersion {
    if (-not (Test-CommandExists "node")) { return $null }
    $out = (& node --version 2>$null) -join ""
    if ($out -match "v?(\d+\.\d+\.\d+)") {
        return $Matches[1]
    }
    return $null
}

Write-Header "TaskVault Developer Environment Doctor & Setup (v1.2.5)"
Write-Host "  [TIP] If you only want to USE TaskVault (without compiling source code)," -ForegroundColor Yellow
Write-Host "        you do NOT need to run this script. Download the ready-to-run installer at:" -ForegroundColor Yellow
Write-Host "        https://github.com/callme707/task_vault/releases/latest" -ForegroundColor Green

Refresh-SessionPath

$wv2 = Test-WebView2Installed
$msvc = Test-MsvcInstalled
$rustcVer = Get-RustcVersion
$tauriVer = Test-TauriCliInstalled
$nodeVer = Get-NodeVersion

$rustcOk = $false
if ($rustcVer) {
    $rustcOk = ([version]$rustcVer -ge [version]"1.77.2")
}

$nodeOk = $false
if ($nodeVer) {
    $nodeOk = ([version]$nodeVer -ge [version]"18.0.0")
}

$checks = @(
    [PSCustomObject]@{
        Component = "WebView2 Runtime"
        Required  = "Edge WebView2"
        Current   = if ($wv2) { "v$wv2" } else { "Not found" }
        Status    = if ($wv2) { "OK" } else { "MISSING" }
    },
    [PSCustomObject]@{
        Component = "MSVC C++ Build Tools"
        Required  = "VC.Tools.x86.x64 + WinSDK"
        Current   = if ($msvc) { $msvc } else { "Not found" }
        Status    = if ($msvc) { "OK" } else { "MISSING" }
    },
    [PSCustomObject]@{
        Component = "Rust Toolchain (rustc)"
        Required  = ">= 1.77.2 (stable-msvc)"
        Current   = if ($rustcVer) { "v$rustcVer" } else { "Not found" }
        Status    = if ($rustcOk) { "OK" } else { "MISSING" }
    },
    [PSCustomObject]@{
        Component = "Tauri CLI (cargo-tauri)"
        Required  = "^2.0.0 (for installer bundling)"
        Current   = if ($tauriVer) { $tauriVer } else { "Not found" }
        Status    = if ($tauriVer) { "OK" } else { "OPTIONAL" }
    },
    [PSCustomObject]@{
        Component = "Node.js Runtime"
        Required  = ">= 18.0.0 (tests & preview)"
        Current   = if ($nodeVer) { "v$nodeVer" } else { "Not found" }
        Status    = if ($nodeOk) { "OK" } else { "MISSING" }
    }
)

Write-Host ""
$checks | Format-Table -AutoSize

$missing = @($checks | Where-Object { $_.Status -ne "OK" })
$hardMissing = @($checks | Where-Object { $_.Status -eq "MISSING" })

if ($CheckOnly) {
    if ($hardMissing.Count -eq 0) {
        Write-Host "[OK] All required build and runtime dependencies are ready!" -ForegroundColor Green
        exit 0
    } else {
        Write-Host "[WARN] Missing required dependencies. Run .\setup-env.bat to install them." -ForegroundColor Yellow
        exit 1
    }
}

if ($missing.Count -gt 0) {
    if (-not $Yes) {
        $reply = Read-Host "Missing or optional dependencies detected. Install automatically via winget/cargo? [Y/n]"
        if ($reply -match "^[Nn]") {
            Write-Host "Skipped automatic installation." -ForegroundColor Yellow
            exit 0
        }
    }

    if (-not (Test-CommandExists "winget")) {
        Write-Host "[ERROR] winget (Windows Package Manager) was not found. Please update App Installer from Microsoft Store." -ForegroundColor Red
        exit 1
    }

    if (-not $wv2) {
        Write-Host "-> Installing Microsoft Edge WebView2 Runtime..." -ForegroundColor Cyan
        & winget install --id Microsoft.EdgeWebView2Runtime -e --accept-source-agreements --accept-package-agreements
    }

    if (-not $msvc) {
        Write-Host "-> Installing Visual Studio 2022 C++ Build Tools (MSVC + Windows SDK)..." -ForegroundColor Cyan
        & winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-source-agreements --accept-package-agreements --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
    }

    if (-not $rustcOk) {
        $rustupCmd = Resolve-CargoTool "rustup"
        if (-not $rustupCmd) {
            Write-Host "-> Installing Rustup..." -ForegroundColor Cyan
            & winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
            $rustupCmd = Resolve-CargoTool "rustup"
        }
        if ($rustupCmd) {
            Write-Host "-> Configuring Rust stable-x86_64-pc-windows-msvc toolchain..." -ForegroundColor Cyan
            & $rustupCmd toolchain install stable-x86_64-pc-windows-msvc --no-self-update
            $rustupCmd = Resolve-CargoTool "rustup"
            & $rustupCmd default stable-x86_64-pc-windows-msvc
            & $rustupCmd component add clippy rustfmt
            Refresh-SessionPath
        } else {
            Write-Host "[WARN] Rustup was installed, but PATH requires a new terminal session. Please re-run setup-env.bat." -ForegroundColor Yellow
            exit 1
        }
    }

    if (-not $nodeOk) {
        Write-Host "-> Installing Node.js LTS..." -ForegroundColor Cyan
        & winget install --id OpenJS.NodeJS.LTS -e --accept-source-agreements --accept-package-agreements
        Refresh-SessionPath
    }

    $cargoCmd = Resolve-CargoTool "cargo"
    if (-not $tauriVer -and $cargoCmd) {
        Write-Host "-> Installing Tauri CLI (tauri-cli ^2.0.0)..." -ForegroundColor Cyan
        & $cargoCmd install tauri-cli --version "^2.0.0" --locked
        Refresh-SessionPath
    }
}

Write-Header "Running Workspace Verification"
$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    if (Test-CommandExists "node") {
        Write-Host "-> Verifying frontend syntax and unit tests..." -ForegroundColor Cyan
        & node --check ui/main.js
        & node --check ui/quick.js
        & node ui/tests/time-format.test.js
        & node ui/tests/task-views.test.js
    }
    $cargoCmd = Resolve-CargoTool "cargo"
    if ($cargoCmd) {
        Write-Host "-> Verifying Rust crate dependencies (cargo check)..." -ForegroundColor Cyan
        Push-Location (Join-Path $repoRoot "src-tauri")
        try {
            & $cargoCmd check --locked
        } finally {
            Pop-Location
        }
    }
    Write-Host ""
    Write-Host "[SUCCESS] TaskVault environment setup and verification passed!" -ForegroundColor Green
    Write-Host "  - Start dev mode:       cd src-tauri && cargo run" -ForegroundColor Gray
    Write-Host "  - Build release binary: cd src-tauri && cargo build --release --locked" -ForegroundColor Gray
    Write-Host "  - UI sandbox preview:   node scripts/preview.cjs" -ForegroundColor Gray
} finally {
    Pop-Location
}
