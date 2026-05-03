# build-staging.ps1 — Windows / PowerShell counterpart of build-staging.sh.
# See that file for rationale.

$ErrorActionPreference = 'Stop'
$envFile = Join-Path $PSScriptRoot '..\.env.staging'

if (-not (Test-Path $envFile)) {
    Write-Error "$envFile missing — run from repo root or fix the path"
    exit 1
}

Get-Content $envFile | ForEach-Object {
    if ($_ -match '^\s*#' -or $_ -notmatch '=') { return }
    $k, $v = $_ -split '=', 2
    Set-Item -Path "env:$k" -Value $v
}

if ($env:DIMMY_LICENSE_PUBKEY -eq 'REPLACE_WITH_STAGING_PUBKEY') {
    Write-Error '.env.staging still has the placeholder pubkey — run the keypair generator in WSL and paste the value in.'
    exit 1
}

Push-Location (Join-Path $PSScriptRoot '..\core')
try {
    & cargo build --release --features license-client @args
} finally {
    Pop-Location
}
