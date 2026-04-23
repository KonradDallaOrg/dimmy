#!/usr/bin/env pwsh
# Resets onboarding state so the next Dimmy launch shows the wizard again.
# Useful when filming onboarding walkthroughs or testing the flow locally.
#
# Flags:
#   -KeepModels   don't delete downloaded whisper models
#   -KeepKey      don't delete the encrypted API key store
#
# Defaults remove only the .onboarding_done marker and any .part files —
# fully downloaded models + saved keys are kept (fastest iteration).

param(
    [switch]$WipeModels,
    [switch]$WipeKey
)

$dimmyDir = Join-Path $env:APPDATA 'dimmy'

if (-not (Test-Path $dimmyDir)) {
    Write-Host "No dimmy config dir at $dimmyDir — nothing to reset." -ForegroundColor Yellow
    exit 0
}

$marker = Join-Path $dimmyDir '.onboarding_done'
if (Test-Path $marker) {
    Remove-Item $marker -Force
    Write-Host "Removed onboarding marker." -ForegroundColor Green
} else {
    Write-Host "Marker already absent." -ForegroundColor DarkGray
}

$modelsDir = Join-Path $dimmyDir 'models'
if (Test-Path $modelsDir) {
    Get-ChildItem $modelsDir -Filter '*.part' -ErrorAction SilentlyContinue |
        ForEach-Object { Remove-Item $_.FullName -Force; Write-Host "Removed partial: $($_.Name)" -ForegroundColor Green }

    if ($WipeModels) {
        Get-ChildItem $modelsDir -Filter '*.bin' -ErrorAction SilentlyContinue |
            ForEach-Object { Remove-Item $_.FullName -Force; Write-Host "Removed model: $($_.Name)" -ForegroundColor Green }
    }
}

if ($WipeKey) {
    $keyFile = Join-Path $dimmyDir 'keys.enc'
    if (Test-Path $keyFile) {
        Remove-Item $keyFile -Force
        Write-Host "Removed keys.enc." -ForegroundColor Green
    }
}

Write-Host ""
Write-Host "Done. Launch Dimmy to see onboarding again." -ForegroundColor Cyan
Write-Host "Flags: -WipeModels (force re-download) | -WipeKey (force re-entry)"
