<#
.SYNOPSIS
  Autonomously capture marketing/docs PNGs of the Windows app: every
  Settings page + the meeting window + the pill, in light AND dark theme.

.DESCRIPTION
  Superset of capture-settings-ui.ps1. Drives the running app purely via
  its own deep-link commands (`--command open-settings:<tag>`,
  `open-meeting`, `toggle-pill`) and captures each window by HWND using
  PrintWindow(PW_RENDERFULLCONTENT) — no focus stealing, no UI clicking.

  Theme loop: the app reads its theme from %APPDATA%\<configdir>\ui_prefs.json
  at startup (no live set-theme command exists), so for each requested
  theme we patch ui_prefs.json, relaunch, and capture the full set. The
  user's original Theme is saved and RESTORED at the end.

  Output: PNGs under docs/ui-shots/win/ (gitignored) named
  `<screen>-<theme>.png`, plus a manifest.json describing every shot
  (screen, theme, os, suggested caption) for the website/docs agents.

  Onboarding + pill recording-states are NOT covered here (they need a
  step-through driver / forced state) — tracked as a follow-up.

.PARAMETER ExePath  Path to Dimmy.Windows.exe. Defaults to the x64 Debug build.
.PARAMETER OutDir   Output folder. Defaults to docs/ui-shots/win under the repo root.
.PARAMETER Theme    Light | Dark | Both. Default Both.
.PARAMETER DelayMs  Settle time after each navigate before capture. Default 900.

.EXAMPLE
  pwsh -File scripts/dev/capture-ui.ps1
  pwsh -File scripts/dev/capture-ui.ps1 -Theme Dark
#>
[CmdletBinding()]
param(
    [string]$ExePath,
    [string]$OutDir,
    [ValidateSet('Light', 'Dark', 'Both')]
    [string]$Theme = 'Both',
    [int]$DelayMs = 900
)

$ErrorActionPreference = 'Stop'

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..')
if (-not $ExePath) {
    $ExePath = Join-Path $repoRoot 'platforms\windows\Dimmy.Windows\bin\x64\Debug\net8.0-windows10.0.19041.0\win-x64\Dimmy.Windows.exe'
}
if (-not $OutDir) { $OutDir = Join-Path $repoRoot 'docs\ui-shots\win' }
if (-not (Test-Path $ExePath)) { throw "Dimmy.Windows.exe not found at $ExePath. Build the x64 Debug host first." }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# ui_prefs.json lives next to config under %APPDATA%\dimmy by default
# (prod/Debug flavor). Staging would be dimmy-staging, but screenshots
# are taken from the Debug build => dimmy.
$prefsPath = Join-Path $env:APPDATA 'dimmy\ui_prefs.json'

# Settings panels reachable by deep link even when Advanced-gated.
$settingsPages = @(
    'home', 'voice', 'output', 'providers', 'pill', 'rules',
    'shortcut', 'history', 'integrations', 'privacy', 'license', 'about', 'advanced'
)

# Human captions for the manifest (website/docs agents consume these).
$captions = @{
    'home'         = 'Dimmy home / dashboard'
    'voice'        = 'Voice input: microphone, STT backend, accuracy options'
    'output'       = 'Output: LLM post-processing, recap, paste behaviour'
    'providers'    = 'Providers and keys: connect Groq, OpenAI, Anthropic, Deepgram, local'
    'pill'         = 'Pill overlay settings'
    'rules'        = 'App rules: per-app LLM style overrides'
    'shortcut'     = 'Global hotkeys for dictation and command mode'
    'history'      = 'Transcription history with search'
    'integrations' = 'Integrations: Notion, MCP, Claude/Codex subscription'
    'privacy'      = 'Privacy and telemetry controls'
    'license'      = 'License and plan'
    'about'        = 'About and updates'
    'advanced'     = 'Advanced settings'
    'meeting'      = 'Meeting mode: long-form recording and AI recap'
    'pill-idle'    = 'The Dimmy pill overlay (idle)'
}

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

function Get-DimmyPids {
    Get-Process -Name 'Dimmy.Windows' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id
}

# Find an owned top-level window. $titled=$true picks the largest window whose
# title matches $pattern; $titled=$false picks a small borderless window with an
# EMPTY title (the pill).
function Find-Window([string]$pattern, [bool]$titled) {
    $pids = Get-DimmyPids
    if (-not $pids) { return [IntPtr]::Zero }
    $script:best = [IntPtr]::Zero; $script:bestArea = 0
    $cb = [WinCap+EnumProc]{
        param($h, $l)
        if (-not [WinCap]::IsWindowVisible($h)) { return $true }
        $wpid = 0; [WinCap]::GetWindowThreadProcessId($h, [ref]$wpid) | Out-Null
        if ($pids -notcontains $wpid) { return $true }
        $sb = New-Object System.Text.StringBuilder 256
        [WinCap]::GetWindowText($h, $sb, 256) | Out-Null
        $title = $sb.ToString()
        $r = New-Object WinCap+RECT
        [WinCap]::GetWindowRect($h, [ref]$r) | Out-Null
        $w = $r.Right - $r.Left; $ht = $r.Bottom - $r.Top
        $area = $w * $ht
        if ($titled) {
            if ([string]::IsNullOrWhiteSpace($title)) { return $true }
            if ($pattern -and $title -notmatch $pattern) { return $true }
            if ($area -gt $script:bestArea) { $script:bestArea = $area; $script:best = $h }
        }
        else {
            # pill: empty title, modest size, on screen
            if (-not [string]::IsNullOrWhiteSpace($title)) { return $true }
            if ($w -le 0 -or $ht -le 0 -or $w -gt 400 -or $ht -gt 200) { return $true }
            if ($area -gt $script:bestArea) { $script:bestArea = $area; $script:best = $h }
        }
        return $true
    }
    [WinCap]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    return $script:best
}

function Capture-Window([IntPtr]$h, [string]$outFile) {
    if ($h -eq [IntPtr]::Zero) { Write-Warning "  window not found for $([System.IO.Path]::GetFileName($outFile))"; return $false }
    $r = New-Object WinCap+RECT
    [WinCap]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left; $ht = $r.Bottom - $r.Top
    if ($w -le 0 -or $ht -le 0) { Write-Warning "  bad rect"; return $false }
    $bmp = New-Object System.Drawing.Bitmap $w, $ht
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    $ok = [WinCap]::PrintWindow($h, $hdc, 2)  # PW_RENDERFULLCONTENT
    $g.ReleaseHdc($hdc)
    if (-not $ok) { $g.Dispose(); $bmp.Dispose(); Write-Warning "  PrintWindow failed"; return $false }
    $bmp.Save($outFile, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    return $true
}

function Set-Theme([string]$mode) {
    # mode: "Light" | "Dark". Patch ui_prefs.json, preserving all other keys.
    if (Test-Path $prefsPath) {
        $json = Get-Content $prefsPath -Raw | ConvertFrom-Json
    }
    else {
        $json = [pscustomobject]@{}
    }
    if ($json.PSObject.Properties.Name -contains 'Theme') { $json.Theme = $mode }
    else { $json | Add-Member -NotePropertyName Theme -NotePropertyValue $mode }
    ($json | ConvertTo-Json -Depth 10) | Set-Content -Path $prefsPath -Encoding utf8
}

function Stop-Dimmy {
    Get-Process -Name 'Dimmy.Windows' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
}

# Remember the user's theme so we can restore it.
$originalTheme = 'Default'
if (Test-Path $prefsPath) {
    try { $originalTheme = (Get-Content $prefsPath -Raw | ConvertFrom-Json).Theme } catch {}
}
if (-not $originalTheme) { $originalTheme = 'Default' }

$themes = if ($Theme -eq 'Both') { @('Light', 'Dark') } else { @($Theme) }
$manifest = @()

try {
    foreach ($t in $themes) {
        $tl = $t.ToLower()
        Write-Host "=== Theme: $t ===" -ForegroundColor Cyan
        Stop-Dimmy
        Set-Theme $t
        Write-Host "Launching app in $t theme..."
        Start-Process -FilePath $ExePath | Out-Null
        Start-Sleep -Seconds 7

        # Settings pages
        Start-Process -FilePath $ExePath -ArgumentList '--command', 'open-settings' | Out-Null
        Start-Sleep -Milliseconds ($DelayMs + 900)
        foreach ($tag in $settingsPages) {
            Start-Process -FilePath $ExePath -ArgumentList '--command', "open-settings:$tag" | Out-Null
            Start-Sleep -Milliseconds $DelayMs
            $hwnd = Find-Window 'Settings' $true
            $name = "settings-$tag-$tl.png"
            $out = Join-Path $OutDir $name
            if (Capture-Window $hwnd $out) {
                Write-Host "  captured $name"
                $manifest += [pscustomobject]@{ file = "win/$name"; screen = $tag; theme = $tl; os = 'windows'; caption = $captions[$tag] }
            }
        }

        # Meeting window
        Start-Process -FilePath $ExePath -ArgumentList '--command', 'open-meeting' | Out-Null
        Start-Sleep -Milliseconds ($DelayMs + 600)
        $hwnd = Find-Window 'Meeting' $true
        if ($hwnd -eq [IntPtr]::Zero) { $hwnd = Find-Window '.' $true }  # fallback: any titled (excluding by area pick)
        $name = "meeting-$tl.png"
        if (Capture-Window $hwnd (Join-Path $OutDir $name)) {
            Write-Host "  captured $name"
            $manifest += [pscustomobject]@{ file = "win/$name"; screen = 'meeting'; theme = $tl; os = 'windows'; caption = $captions['meeting'] }
        }

        # Pill (idle) — best effort: borderless transparent window, PrintWindow
        # may not preserve rounded transparency; flagged for follow-up tuning.
        Start-Process -FilePath $ExePath -ArgumentList '--command', 'toggle-pill' | Out-Null
        Start-Sleep -Milliseconds ($DelayMs + 400)
        $hwnd = Find-Window '' $false
        $name = "pill-idle-$tl.png"
        if (Capture-Window $hwnd (Join-Path $OutDir $name)) {
            Write-Host "  captured $name (best-effort)"
            $manifest += [pscustomobject]@{ file = "win/$name"; screen = 'pill-idle'; theme = $tl; os = 'windows'; caption = $captions['pill-idle'] }
        }
        Start-Process -FilePath $ExePath -ArgumentList '--command', 'toggle-pill' | Out-Null  # hide again
        Start-Sleep -Milliseconds 300
    }
}
finally {
    # Restore the user's original theme and relaunch once so they're back
    # to where they started.
    Write-Host "Restoring theme to $originalTheme..."
    Stop-Dimmy
    Set-Theme $originalTheme
}

# Write manifest
$manifestPath = Join-Path $OutDir 'manifest.json'
($manifest | ConvertTo-Json -Depth 6) | Set-Content -Path $manifestPath -Encoding utf8

Write-Host ""
Write-Host "Done. $($manifest.Count) shots + manifest.json in $OutDir" -ForegroundColor Green
