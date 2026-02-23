# Vocino - Windows Build Script
#
# IMPORTANT: Run from a NATIVE Windows terminal (not from WSL)
#
# Option 1: Open PowerShell on Windows, then:
#   cd \\wsl$\Ubuntu\home\konrad\code\pai-voice
#   powershell -ExecutionPolicy Bypass -File build-windows.ps1
#
# Option 2: Open "x64 Native Tools Command Prompt for VS 2022", then:
#   powershell -ExecutionPolicy Bypass -File \\wsl$\Ubuntu\home\konrad\code\pai-voice\build-windows.ps1

$ErrorActionPreference = 'Stop'

$WslSource = '\\wsl$\Ubuntu\home\konrad\code\pai-voice'
$BuildDir = Join-Path $env:USERPROFILE 'pai-voice-build'
$CargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

# Ensure cargo bin is in PATH
$env:Path = $CargoBin + ';' + $env:Path

Write-Host ''
Write-Host '=== Vocino - Windows Build ===' -ForegroundColor Cyan

# Step 0: Setup MSVC environment if link.exe not found
$linkExe = Get-Command link.exe -ErrorAction SilentlyContinue
if (-not $linkExe) {
    Write-Host '[0/4] Setting up MSVC environment...' -ForegroundColor Yellow

    # Find vcvarsall.bat
    $vsWhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vsWhere) {
        # Try with -products * to find Build Tools as well as full VS
        $vsPath = & $vsWhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if (-not $vsPath) {
            $vsPath = & $vsWhere -latest -products * -property installationPath
        }
        $vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
        if (Test-Path $vcvars) {
            Write-Host ('  Loading: ' + $vcvars) -ForegroundColor Yellow
            # Run vcvars64.bat and capture the resulting environment
            # Push to a local path first — cmd.exe cannot handle UNC paths
            $tempFile = [System.IO.Path]::GetTempFileName()
            Push-Location $env:USERPROFILE
            cmd.exe /c "`"$vcvars`" >nul 2>&1 && set > `"$tempFile`""
            Pop-Location
            Get-Content $tempFile | ForEach-Object {
                if ($_ -match '^([^=]+)=(.*)$') {
                    [System.Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
                }
            }
            Remove-Item $tempFile -Force
            Write-Host '  MSVC environment loaded.' -ForegroundColor Green
        } else {
            Write-Host '  ERROR: vcvars64.bat not found. Install VS Build Tools with C++ workload.' -ForegroundColor Red
            exit 1
        }
    } else {
        Write-Host '  ERROR: vswhere.exe not found. Install Visual Studio Build Tools.' -ForegroundColor Red
        exit 1
    }
}

# Step 1: Check Rust
Write-Host '[1/4] Checking Rust...' -ForegroundColor Yellow
$rustcExe = Join-Path $CargoBin 'rustc.exe'
if (-not (Test-Path $rustcExe)) {
    Write-Host '  Rust not found. Install from https://rustup.rs' -ForegroundColor Red
    exit 1
}
$ver = & $rustcExe --version
Write-Host ('  ' + $ver) -ForegroundColor Green

# Step 2: Check/install Tauri CLI
Write-Host '[2/4] Checking Tauri CLI...' -ForegroundColor Yellow
$cargoExe = Join-Path $CargoBin 'cargo.exe'
$tauriExe = Join-Path $CargoBin 'cargo-tauri.exe'
if (-not (Test-Path $tauriExe)) {
    Write-Host '  Installing Tauri CLI (takes a few minutes)...' -ForegroundColor Yellow
    & $cargoExe install tauri-cli --version '^2'
    if ($LASTEXITCODE -ne 0) {
        Write-Host '  Failed to install Tauri CLI' -ForegroundColor Red
        exit 1
    }
}
Write-Host '  Tauri CLI ready.' -ForegroundColor Green

# Step 3: Sync source to Windows disk
Write-Host '[3/4] Syncing source...' -ForegroundColor Yellow

$srcFe = Join-Path $BuildDir 'src'
$srcTauri = Join-Path $BuildDir 'src-tauri'

New-Item -ItemType Directory -Force -Path (Join-Path $srcTauri 'src') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $srcTauri 'capabilities') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $srcTauri 'icons') | Out-Null
New-Item -ItemType Directory -Force -Path $srcFe | Out-Null

Copy-Item (Join-Path $WslSource 'src\*') $srcFe -Recurse -Force
Copy-Item (Join-Path $WslSource 'src-tauri\src\*') (Join-Path $srcTauri 'src') -Recurse -Force
Copy-Item (Join-Path $WslSource 'src-tauri\Cargo.toml') $srcTauri -Force
Copy-Item (Join-Path $WslSource 'src-tauri\build.rs') $srcTauri -Force
Copy-Item (Join-Path $WslSource 'src-tauri\tauri.conf.json') $srcTauri -Force
Copy-Item (Join-Path $WslSource 'src-tauri\capabilities\*') (Join-Path $srcTauri 'capabilities') -Force
Copy-Item (Join-Path $WslSource 'src-tauri\icons\*') (Join-Path $srcTauri 'icons') -Force
Write-Host ('  Done: ' + $BuildDir) -ForegroundColor Green

# Step 4: Build
Write-Host '[4/4] Building Vocino...' -ForegroundColor Yellow
Push-Location $BuildDir
& $cargoExe tauri build
$result = $LASTEXITCODE
Pop-Location

if ($result -ne 0) {
    Write-Host 'BUILD FAILED' -ForegroundColor Red
    exit 1
}

# Report
# Try Vocino.exe first (new name), fall back to pai-voice.exe
$exePath = Join-Path $srcTauri 'target\release\Vocino.exe'
if (-not (Test-Path $exePath)) {
    $exePath = Join-Path $srcTauri 'target\release\pai-voice.exe'
}
if (Test-Path $exePath) {
    $sizeMB = [math]::Round((Get-Item $exePath).Length / 1MB, 1)
    Write-Host ''
    Write-Host '=== BUILD SUCCESS ===' -ForegroundColor Green
    Write-Host ('  EXE: ' + $exePath) -ForegroundColor White
    Write-Host ('  Size: ' + $sizeMB + ' MB') -ForegroundColor White
    $desktop = [Environment]::GetFolderPath('Desktop')
    $destExe = Join-Path $desktop 'Vocino.exe'
    # Kill running instance before copying
    Get-Process -Name 'Vocino' -ErrorAction SilentlyContinue | Stop-Process -Force
    Get-Process -Name 'pai-voice' -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
    Copy-Item $exePath $destExe -Force
    Write-Host ('  Copied to Desktop: ' + $destExe) -ForegroundColor Green
    Write-Host ''
    Write-Host '  Launch: double-click Vocino.exe on Desktop' -ForegroundColor Cyan
    Write-Host '  Hotkey: Win+Alt to toggle recording' -ForegroundColor Cyan
}
