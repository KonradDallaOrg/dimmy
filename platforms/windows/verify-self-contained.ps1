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
    # Parakeet ort runtime: required by `local-stt-parakeet` feature.
    # Without these the Rust DLL exports `dimmy_parakeet_*` but every
    # call returns LocalModel("ort init failed") at runtime — backend
    # silently broken on first use.
    'onnxruntime.dll',
    'onnxruntime_providers_shared.dll'
    # VC++ runtime (msvcp140/vcruntime140) is NOT bundled here — Velopack
    # installs it at setup time via `--framework vcredist143-x64` (the
    # official Microsoft Redist, which lands in System32). Co-locating our
    # own copy is redundant and historically caused ABI mismatches when the
    # bundled version was older than System32.
)

# Directories that must exist (WinAppSDK ships XAML controls here)
$requiredDirs = @(
    'Microsoft.UI.Xaml'
)

# The APP's own PRI file (resources.pri) must exist at root. MRT Core uses it
# to resolve ms-appx:///Views/*.xaml URIs at InitializeComponent() time.
# Without it, every Window throws XamlParseException and the app runs headless
# (the try/catch in App.OnLaunched swallows the error => process stays alive
# but no pill, no tray). Do NOT relax this check to "any .pri" -- the
# WindowsAppSDK ships Microsoft.UI.pri + Microsoft.UI.Xaml.Controls.pri by
# default and those would give a false positive while the app's resources.pri
# is missing. Bug: staging-latest 2026-04-18 shipped without app PRI because
# Directory.Build.targets stubbed MrtCore targets.
$priFiles = @(Get-ChildItem -Path $Path -Filter '*.pri' -File -ErrorAction SilentlyContinue)
$appPri = $priFiles | Where-Object { $_.Name -eq 'resources.pri' -or $_.Name -eq 'Dimmy.Windows.pri' }

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
if (-not $appPri) {
    Write-Host ""
    Write-Host "App's own PRI (resources.pri or Dimmy.Windows.pri) NOT FOUND at root." -ForegroundColor Red
    Write-Host "The .pri files present are:" -ForegroundColor Red
    foreach ($f in $priFiles) {
        $kb = [math]::Round($f.Length / 1KB, 1)
        Write-Host "  - $($f.Name) ($kb KB)" -ForegroundColor Red
    }
    Write-Host ""
    Write-Host "Microsoft.UI.pri / Microsoft.UI.Xaml.Controls.pri are NOT the app PRI --" -ForegroundColor Red
    Write-Host "they ship with WindowsAppSDK and index only MUX controls, NOT Views/*.xbf." -ForegroundColor Red
    Write-Host "At runtime, Application.LoadComponent('ms-appx:///Views/OnboardingWindow.xaml')" -ForegroundColor Red
    Write-Host "will throw XamlParseException. Process stays alive (try/catch swallows) but" -ForegroundColor Red
    Write-Host "NO WINDOW EVER SHOWS. This is the 2026-04-18 regression." -ForegroundColor Red
    Write-Host ""
    Write-Host "Fix: csproj must have EnableMsixTooling=true and AppxMSBuildToolsPath must" -ForegroundColor Red
    Write-Host "point at a VS install with the UWP workload (contains Microsoft.Build.Packaging.Pri.Tasks.dll)." -ForegroundColor Red
    $failed = $true
}

if ($failed) {
    Write-Host ""
    Write-Host "Self-contained check FAILED." -ForegroundColor Red
    Write-Host ""
    Write-Host "Fix hints:"
    Write-Host "  - Use 'dotnet publish -r win-x64 --self-contained' (not 'dotnet build')"
    Write-Host "  - 'dotnet build' does NOT honor <SelfContained>/<WindowsAppSDKSelfContained>"
    Write-Host "  - VC++ runtime is installed by Velopack (--framework vcredist143-x64), not bundled here"
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
