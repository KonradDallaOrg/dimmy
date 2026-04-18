# Verifies a Dimmy.Windows publish folder is truly self-contained.
# Fails (exit 1) if any critical DLL / resource is missing so CI catches broken
# bundles before they reach users on clean Windows machines (no .NET, no WinAppSDK).
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
# to launch on a clean Windows 11 machine.
$requiredFiles = @(
    # .NET 8 runtime (self-contained)
    'coreclr.dll',
    'hostfxr.dll',
    'hostpolicy.dll',
    'System.Private.CoreLib.dll',
    # WinAppSDK native (WindowsAppSDKSelfContained=true)
    'Microsoft.UI.Xaml.dll',
    'Microsoft.WindowsAppRuntime.dll',
    'Microsoft.WindowsAppRuntime.Bootstrap.dll',
    'Microsoft.Internal.FrameworkUdk.dll',
    'Microsoft.UI.Xaml.Internal.dll',
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

# Directories that must exist (WinAppSDK ships XAML controls here)
$requiredDirs = @(
    'Microsoft.UI.Xaml'
)

# At least ONE .pri file must exist at root — MRT Core uses it for XAML resource
# resolution. Without it, App.InitializeComponent() throws 0xc000027b silently.
$priFiles = @(Get-ChildItem -Path $Path -Filter '*.pri' -File -ErrorAction SilentlyContinue)

$files = Get-ChildItem -Path $Path -Recurse -File | ForEach-Object { $_.Name } | Sort-Object -Unique
$missingFiles = @()
foreach ($name in $requiredFiles) {
    if ($files -notcontains $name) { $missingFiles += $name }
}

$missingDirs = @()
foreach ($name in $requiredDirs) {
    $dirPath = Join-Path $Path $name
    if (-not (Test-Path $dirPath -PathType Container)) { $missingDirs += $name }
    elseif (-not (Get-ChildItem -Path $dirPath -Recurse -File | Select-Object -First 1)) {
        $missingDirs += "$name (empty)"
    }
}

$totalSize = (Get-ChildItem -Path $Path -Recurse -File | Measure-Object Length -Sum).Sum
$sizeMB = [math]::Round($totalSize / 1MB, 1)
Write-Host "Bundle: $Path"
Write-Host "Files: $($files.Count), Total: $sizeMB MB"
Write-Host ".pri files at root: $($priFiles.Count)"

$failed = $false

if ($missingFiles.Count -gt 0) {
    Write-Host ""
    Write-Host "Missing required DLLs:" -ForegroundColor Red
    $missingFiles | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    $failed = $true
}

if ($missingDirs.Count -gt 0) {
    Write-Host ""
    Write-Host "Missing required directories:" -ForegroundColor Red
    $missingDirs | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    $failed = $true
}

if ($priFiles.Count -eq 0) {
    Write-Host ""
    Write-Host "No .pri file found at root." -ForegroundColor Red
    Write-Host "MrtCore PRI generation is disabled or failing. Without a PRI file," -ForegroundColor Red
    Write-Host "WinUI XAML metadata provider throws 0xc000027b and the app dies silently." -ForegroundColor Red
    Write-Host "Check csproj: MrtCoreGenPriFileEnabled must NOT be false." -ForegroundColor Red
    $failed = $true
}

if ($failed) {
    Write-Host ""
    Write-Host "Self-contained check FAILED." -ForegroundColor Red
    Write-Host ""
    Write-Host "Fix hints:"
    Write-Host "  - Use 'dotnet publish -r win-x64 --self-contained' (not 'dotnet build')"
    Write-Host "  - 'dotnet build' does NOT honor <SelfContained>/<WindowsAppSDKSelfContained>"
    Write-Host "  - VC++ runtime DLLs must be copied from MSVC toolchain (see CI workflow)"
    Write-Host "  - PRI files require MrtCoreGenPriFileEnabled=true (or unset)"
    exit 1
}

# Self-contained bundles are ~200 MB+; < 100 MB means something's off.
if ($sizeMB -lt 100) {
    Write-Error "Bundle size suspiciously small ($sizeMB MB). Self-contained builds should be >100 MB."
    exit 1
}

Write-Host ""
Write-Host "OK: bundle is self-contained." -ForegroundColor Green
