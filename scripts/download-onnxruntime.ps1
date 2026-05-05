#requires -version 5.1
<#
.SYNOPSIS
    Download + extract a pinned onnxruntime native binary into core/target.

.DESCRIPTION
    Fetches onnxruntime-win-x64-<ver>.zip from GitHub releases (microsoft/onnxruntime),
    caches it under $env:LOCALAPPDATA\onnxruntime, and extracts to a known path that
    the .csproj CopyOrtDll target reads from.

    Pin version MUST match the ort crate's bundled-version constant. ort 2.0.0-rc.10
    targets onnxruntime 1.22.0 — bumping ort = bumping this script.

    Idempotent: skips download + extract when the destination DLL already exists.
    Safe to call from local dev and from CI (cache dir survives across runs).

.PARAMETER Version
    Pinned onnxruntime version. Default 1.22.0 (ort 2.0.0-rc.10).

.PARAMETER Force
    Re-download even if cached.
#>
param(
    [string]$Version = "1.22.0",
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$targetDir = Join-Path $repoRoot "core\target\onnxruntime-win-x64-$Version"
$libDir = Join-Path $targetDir "lib"
$dllPath = Join-Path $libDir "onnxruntime.dll"

if ((Test-Path $dllPath) -and -not $Force) {
    Write-Host "[ort] cached: $dllPath" -ForegroundColor DarkGray
    exit 0
}

$cacheDir = Join-Path $env:LOCALAPPDATA "onnxruntime"
New-Item -ItemType Directory -Force -Path $cacheDir | Out-Null

$zipName = "onnxruntime-win-x64-$Version.zip"
$zipPath = Join-Path $cacheDir $zipName

if (-not (Test-Path $zipPath) -or $Force) {
    $url = "https://github.com/microsoft/onnxruntime/releases/download/v$Version/$zipName"
    Write-Host "[ort] downloading $url" -ForegroundColor Cyan
    # Use TLS 1.2 explicitly — Windows PowerShell 5.1 default sometimes
    # negotiates TLS 1.0 against GitHub which now refuses it.
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing
}

# Extract to core/target/onnxruntime-win-x64-<ver>/. The zip contains a
# top-level dir already (onnxruntime-win-x64-<ver>/), so extract one level up.
$extractRoot = Join-Path $repoRoot "core\target"
if (Test-Path $targetDir) {
    Remove-Item -Recurse -Force $targetDir
}
Write-Host "[ort] extracting to $targetDir" -ForegroundColor Cyan
Expand-Archive -Path $zipPath -DestinationPath $extractRoot -Force

if (-not (Test-Path $dllPath)) {
    Write-Error "expected DLL not found after extract: $dllPath"
    exit 1
}

$sizeMB = [math]::Round((Get-Item $dllPath).Length / 1MB, 1)
Write-Host "[ort] ready: $dllPath ($sizeMB MB)" -ForegroundColor Green
