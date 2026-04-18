# Runs the Dimmy Velopack installer inside Windows Sandbox for clean-machine testing.
#
# Requirements:
#   - Windows 11 Pro/Enterprise (Home does not ship Windows Sandbox)
#   - Windows Sandbox feature enabled. If not:
#       DISM /Online /Enable-Feature /FeatureName:Containers-DisposableClientVM /All
#       (run as admin, then reboot)
#   - Virtualization enabled in BIOS (VT-x / AMD-V)
#
# Usage:
#   ./test-in-sandbox.ps1                         # auto-find Setup.exe in ./velopack-output
#   ./test-in-sandbox.ps1 -SetupPath <path>       # explicit installer path
#   ./test-in-sandbox.ps1 -PortablePath <path>    # run portable zip instead of installer
#
# What happens inside the sandbox:
#   1. velopack-output/ is mounted read-only at C:\Dimmy
#   2. Setup.exe is launched automatically on logon
#   3. The sandbox is a fresh Windows 11 with NO .NET, NO WinAppSDK, NO VC++ runtime —
#      the only way the app can work is if the bundle is truly self-contained.
#   4. After launch, check %TEMP%\dimmy_startup.log inside the sandbox for stage trace.
#      If the app silently dies, the log tells you WHERE (VelopackApp? ComWrappers?
#      App() ctor?). Open Event Viewer → Windows Logs → Application for faulting module.

[CmdletBinding(DefaultParameterSetName = 'Installer')]
param(
    [Parameter(ParameterSetName = 'Installer')]
    [string]$SetupPath,

    [Parameter(ParameterSetName = 'Portable', Mandatory = $true)]
    [string]$PortablePath
)

$ErrorActionPreference = 'Stop'

# Verify Windows Sandbox is available
$sandboxExe = "$env:SystemRoot\System32\WindowsSandbox.exe"
if (-not (Test-Path $sandboxExe)) {
    Write-Host "Windows Sandbox is NOT installed." -ForegroundColor Red
    Write-Host ""
    Write-Host "Enable it (requires admin PowerShell + reboot):"
    Write-Host "  DISM /Online /Enable-Feature /FeatureName:Containers-DisposableClientVM /All"
    Write-Host ""
    Write-Host "Requirements: Win11 Pro/Enterprise, virtualization enabled in BIOS, 4GB+ RAM."
    exit 1
}

# Resolve input path
if ($PSCmdlet.ParameterSetName -eq 'Portable') {
    $target = Resolve-Path -LiteralPath $PortablePath -ErrorAction Stop
    $hostFolder = Split-Path -Parent $target.Path
    $fileName = Split-Path -Leaf $target.Path
    $command = "powershell -Command `"Expand-Archive C:\Dimmy\$fileName C:\DimmyPortable -Force; Start-Process C:\DimmyPortable\Dimmy.Windows.exe`""
    Write-Host "Mode: portable zip"
}
else {
    if (-not $SetupPath) {
        $default = Join-Path $PSScriptRoot 'velopack-output\Dimmy-win-Setup.exe'
        if (Test-Path $default) {
            $SetupPath = $default
        } else {
            Write-Host "Setup.exe not found at $default" -ForegroundColor Red
            Write-Host "Pass -SetupPath explicitly, or build first via CI / local publish."
            exit 1
        }
    }
    $target = Resolve-Path -LiteralPath $SetupPath -ErrorAction Stop
    $hostFolder = Split-Path -Parent $target.Path
    $fileName = Split-Path -Leaf $target.Path
    $command = "cmd /c start C:\Dimmy\$fileName"
    Write-Host "Mode: installer"
}

Write-Host "Host folder:  $hostFolder"
Write-Host "Sandbox path: C:\Dimmy\$fileName"

$wsbPath = Join-Path $env:TEMP 'dimmy-sandbox.wsb'
$wsb = @"
<Configuration>
  <VGpu>Enable</VGpu>
  <Networking>Enable</Networking>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$hostFolder</HostFolder>
      <SandboxFolder>C:\Dimmy</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>$command</Command>
  </LogonCommand>
</Configuration>
"@

Set-Content -LiteralPath $wsbPath -Value $wsb -Encoding UTF8
Write-Host "WSB config:   $wsbPath"
Write-Host ""
Write-Host "Launching Windows Sandbox..." -ForegroundColor Cyan
Start-Process $sandboxExe -ArgumentList $wsbPath

Write-Host ""
Write-Host "Once inside the sandbox:" -ForegroundColor Yellow
Write-Host "  - Installer/app should auto-launch on logon"
Write-Host "  - If silent fail, open: %TEMP%\dimmy_startup.log"
Write-Host "  - Also check: Event Viewer -> Windows Logs -> Application (ID 1000/1001)"
Write-Host "  - For DLL-loading diagnosis, run ProcMon.exe inside the sandbox"
