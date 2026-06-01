<#
.SYNOPSIS
  Autonomously capture a PNG of every Settings page in the Windows app.

.DESCRIPTION
  Launches (or reuses) Dimmy.Windows.exe and walks every Settings nav page by
  sending the app's own deep-link command `--command open-settings:<tag>`, which
  navigates the Settings window to the named page. We then capture the Settings
  window directly by its HWND using PrintWindow(PW_RENDERFULLCONTENT), so it
  works even when the window is behind the terminal (no focus stealing, no UI
  automation / clicking needed).

  Output: one PNG per page in the output folder, e.g. 01-home.png, 02-voice.png.

.PARAMETER ExePath  Path to Dimmy.Windows.exe. Defaults to the local x64 Debug build.
.PARAMETER OutDir   Folder for the PNGs. Defaults to docs/ui-shots under the repo root.
.PARAMETER DelayMs  Wait after each navigate command before capturing. Default 900.

.EXAMPLE
  pwsh -File scripts/dev/capture-settings-ui.ps1
#>
[CmdletBinding()]
param(
    [string]$ExePath,
    [string]$OutDir,
    [int]$DelayMs = 900
)

$ErrorActionPreference = 'Stop'

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..')
if (-not $ExePath) {
    $ExePath = Join-Path $repoRoot 'platforms\windows\Dimmy.Windows\bin\x64\Debug\net8.0-windows10.0.19041.0\win-x64\Dimmy.Windows.exe'
}
if (-not $OutDir) { $OutDir = Join-Path $repoRoot 'docs\ui-shots' }
if (-not (Test-Path $ExePath)) { throw "Dimmy.Windows.exe not found at $ExePath. Build the x64 Debug host first." }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# Panels are reachable by deep link even when their nav item is Advanced-gated.
$pages = @(
    'home', 'voice', 'output', 'providers', 'pill', 'rules',
    'shortcut', 'history', 'integrations', 'privacy', 'license', 'about', 'advanced'
)

Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class WinCap {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    public struct RECT { public int Left, Top, Right, Bottom; }
}
'@
Add-Type -AssemblyName System.Drawing

# Find the Dimmy Settings window HWND (largest titled top-level window owned by a
# Dimmy.Windows process — the pill is borderless/untitled, the settings window
# has a real title bar).
function Find-SettingsWindow {
    $pids = (Get-Process -Name 'Dimmy.Windows' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
    if (-not $pids) { return [IntPtr]::Zero }
    $best = [IntPtr]::Zero; $bestArea = 0
    $cb = [WinCap+EnumProc]{
        param($h, $l)
        if (-not [WinCap]::IsWindowVisible($h)) { return $true }
        $wpid = 0; [WinCap]::GetWindowThreadProcessId($h, [ref]$wpid) | Out-Null
        if ($pids -notcontains $wpid) { return $true }
        $sb = New-Object System.Text.StringBuilder 256
        [WinCap]::GetWindowText($h, $sb, 256) | Out-Null
        $title = $sb.ToString()
        if ([string]::IsNullOrWhiteSpace($title)) { return $true }
        $r = New-Object WinCap+RECT
        [WinCap]::GetWindowRect($h, [ref]$r) | Out-Null
        $area = ($r.Right - $r.Left) * ($r.Bottom - $r.Top)
        if ($title -match 'Settings' ) { $area += 100000000 }  # prefer the titled Settings window
        if ($area -gt $script:bestArea) { $script:bestArea = $area; $script:best = $h }
        return $true
    }
    $script:best = [IntPtr]::Zero; $script:bestArea = 0
    [WinCap]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    return $script:best
}

function Capture-Window([IntPtr]$h, [string]$outFile) {
    if ($h -eq [IntPtr]::Zero) { Write-Warning "settings window not found"; return $false }
    $r = New-Object WinCap+RECT
    [WinCap]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left; $ht = $r.Bottom - $r.Top
    if ($w -le 0 -or $ht -le 0) { Write-Warning "bad rect"; return $false }
    $bmp = New-Object System.Drawing.Bitmap $w, $ht
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    # PW_RENDERFULLCONTENT = 2 — required for DWM/WinUI composed windows.
    $ok = [WinCap]::PrintWindow($h, $hdc, 2)
    $g.ReleaseHdc($hdc)
    if (-not $ok) { $g.Dispose(); $bmp.Dispose(); Write-Warning "PrintWindow failed"; return $false }
    $bmp.Save($outFile, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    return $true
}

if (-not (Get-Process -Name 'Dimmy.Windows' -ErrorAction SilentlyContinue)) {
    Write-Host "Launching app…"
    Start-Process -FilePath $ExePath | Out-Null
    Start-Sleep -Seconds 6
}
Start-Process -FilePath $ExePath -ArgumentList '--command', 'open-settings' | Out-Null
Start-Sleep -Milliseconds ($DelayMs + 800)

$i = 0
foreach ($tag in $pages) {
    $i++
    Start-Process -FilePath $ExePath -ArgumentList '--command', "open-settings:$tag" | Out-Null
    Start-Sleep -Milliseconds $DelayMs
    $hwnd = Find-SettingsWindow
    $name = '{0:D2}-{1}.png' -f $i, $tag
    $out = Join-Path $OutDir $name
    if (Capture-Window $hwnd $out) { Write-Host "  captured $name" } else { Write-Host "  FAILED  $name" }
}

Write-Host ""
Write-Host "Done. $i shots in $OutDir"
