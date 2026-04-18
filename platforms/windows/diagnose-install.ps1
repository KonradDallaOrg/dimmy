# Diagnose a Dimmy Windows install that starts but shows no UI.
# Run as the SAME user who will launch the app (NOT admin) on the target machine.
# Copy the full output back to the dev.

$ErrorActionPreference = 'Continue'

function Section($name) { Write-Host ""; Write-Host ("=== {0} ===" -f $name) -ForegroundColor Cyan }

Section "OS + user context"
Write-Host "User:    $env:USERNAME"
Write-Host "Domain:  $env:USERDOMAIN"
Write-Host "TEMP:    $env:TEMP"
Write-Host "LOCALAPPDATA: $env:LOCALAPPDATA"
Write-Host "APPDATA: $env:APPDATA"
(Get-CimInstance Win32_OperatingSystem) | Select-Object Caption, Version, BuildNumber, OSArchitecture | Format-List

Section "Install locations (look EVERYWHERE)"
$candidates = @(
    "$env:LOCALAPPDATA\Dimmy",
    "C:\Dimmy",
    "D:\Dimmy",
    "E:\Dimmy",
    "E:\Program Files\Dimmy",
    "E:\Programmi\Dimmy",
    "$env:ProgramFiles\Dimmy",
    "${env:ProgramFiles(x86)}\Dimmy"
)
foreach ($c in $candidates) {
    if (Test-Path $c) {
        Write-Host "FOUND: $c" -ForegroundColor Green
        Get-ChildItem $c -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '\.(exe|dll|pri|json)$' } |
            Select-Object FullName, Length | Format-Table -AutoSize
    }
}
# Also search wide
Write-Host "--- wide search for Dimmy.Windows.exe ---"
Get-ChildItem -Path C:\,D:\,E:\ -Filter Dimmy.Windows.exe -Recurse -ErrorAction SilentlyContinue -Force |
    Select-Object FullName, Length, LastWriteTime | Format-Table -AutoSize

Section "Running Dimmy-related processes"
Get-Process | Where-Object { $_.ProcessName -match 'Dimmy|Velopack|Update|Setup' } |
    Select-Object Id, ProcessName, Path, StartTime | Format-Table -AutoSize

Section "Startup log candidates"
$logs = @(
    "$env:TEMP\dimmy_startup.log",
    "$env:LOCALAPPDATA\Temp\dimmy_startup.log",
    "$env:APPDATA\dimmy\dimmy.log",
    "$env:APPDATA\dimmy\crash.log",
    "$env:APPDATA\dimmy\ptt.log"
)
foreach ($l in $logs) {
    Write-Host "-- $l --"
    if (Test-Path $l) {
        $info = Get-Item $l
        Write-Host ("size={0} bytes, mtime={1}" -f $info.Length, $info.LastWriteTime)
        Get-Content $l -Tail 80
    } else {
        Write-Host "(missing)"
    }
}

Section "Single-instance mutex holder"
# If mutex exists but no visible Dimmy, a zombie owns it
$mutex = $null
try {
    $mutex = [System.Threading.Mutex]::OpenExisting("Global\DimmySingleInstance")
    Write-Host "Global\DimmySingleInstance EXISTS — another process holds it" -ForegroundColor Yellow
    $mutex.Dispose()
} catch {
    Write-Host "Global\DimmySingleInstance not present (no blocker)"
}

Section "Recent App Error events (last 10 min)"
$events = Get-WinEvent -FilterHashtable @{
    LogName='Application'
    StartTime=(Get-Date).AddMinutes(-10)
} -ErrorAction SilentlyContinue |
    Where-Object { $_.Message -match 'Dimmy|\.NET|coreclr|WinAppSDK|WindowsAppRuntime|vcruntime' }
if ($events) {
    $events | Select-Object TimeCreated, LevelDisplayName, ProviderName, Id |
        Format-Table -AutoSize
    Write-Host "--- full messages ---"
    $events | ForEach-Object {
        Write-Host ("[{0}] {1} / {2}" -f $_.TimeCreated, $_.ProviderName, $_.Id)
        Write-Host $_.Message
        Write-Host "---"
    }
} else {
    Write-Host "(nothing matching)"
}

Section "WindowsAppRuntime + VC++ state"
Get-ChildItem "C:\Program Files\WindowsApps" -Filter "Microsoft.WindowsAppRuntime*" -ErrorAction SilentlyContinue |
    Select-Object Name | Format-Table -AutoSize
Get-ChildItem "C:\Windows\System32\vcruntime140*.dll","C:\Windows\System32\msvcp140*.dll" -ErrorAction SilentlyContinue |
    Select-Object Name | Format-Table -AutoSize

Section "Mic privacy setting (Windows 11 blocks silently if off)"
$mic = Get-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone" -ErrorAction SilentlyContinue
if ($mic) { Write-Host ("microphone consent (user): {0}" -f $mic.Value) }
$mic2 = Get-ItemProperty -Path "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone" -ErrorAction SilentlyContinue
if ($mic2) { Write-Host ("microphone consent (machine): {0}" -f $mic2.Value) }

Section "Done"
Write-Host "Copy ALL output above and send it back."
