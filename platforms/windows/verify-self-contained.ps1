# Verifies a Dimmy.Windows publish folder is truly self-contained.
# Fails (exit 1) if any critical DLL is missing so CI catches broken bundles
# before they reach users on clean Windows machines.
param(
    [Parameter(Mandatory = $true)]
    [string]$Path
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $Path)) {
    Write-Error "Path not found: $Path"
    exit 1
}

# Files that MUST be present for an unpackaged, self-contained WinUI 3 + .NET 8 app
# to launch on a clean Windows machine (no .NET runtime, no WinAppSDK installed).
$required = @(
    # .NET 8 runtime (self-contained)
    'coreclr.dll',
    'hostfxr.dll',
    'hostpolicy.dll',
    'System.Private.CoreLib.dll',
    # WinAppSDK native (WindowsAppSDKSelfContained=true)
    'Microsoft.UI.Xaml.dll',
    'Microsoft.WindowsAppRuntime.dll',
    'Microsoft.WindowsAppRuntime.Bootstrap.dll',
    'CoreMessagingXP.dll',
    'DWriteCore.dll',
    'Microsoft.InputStateManager.dll',
    # App + native
    'Dimmy.Windows.exe',
    'Dimmy.Windows.dll',
    'dimmy_lib.dll',
    # VC++ runtime (needed by Rust DLL — without these, app silently exits on clean machines)
    'vcruntime140.dll',
    'msvcp140.dll'
)

$files = Get-ChildItem -Path $Path -Recurse -File | ForEach-Object { $_.Name } | Sort-Object -Unique
$missing = @()
foreach ($name in $required) {
    if ($files -notcontains $name) { $missing += $name }
}

$totalSize = (Get-ChildItem -Path $Path -Recurse -File | Measure-Object Length -Sum).Sum
$sizeMB = [math]::Round($totalSize / 1MB, 1)
Write-Host "Bundle: $Path"
Write-Host "Files: $($files.Count), Total: $sizeMB MB"

if ($missing.Count -gt 0) {
    Write-Host ""
    Write-Host "Self-contained check FAILED. Missing DLLs:" -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    Write-Host ""
    Write-Host "Did you use 'dotnet publish -r win-x64 --self-contained'?"
    Write-Host "'dotnet build' does NOT honor <SelfContained> / <WindowsAppSDKSelfContained>."
    Write-Host "VC++ runtime DLLs must be copied from MSVC toolchain (see CI workflow)."
    exit 1
}

# Self-contained bundles are ~200 MB+; < 100 MB means something's off.
if ($sizeMB -lt 100) {
    Write-Error "Bundle size suspiciously small ($sizeMB MB). Self-contained builds should be >100 MB."
    exit 1
}

Write-Host ""
Write-Host "OK: bundle is self-contained." -ForegroundColor Green
