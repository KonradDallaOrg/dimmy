<#
.SYNOPSIS
    Firma (Authenticode) l'installer Windows di una release GitHub e lo ricarica firmato.

.DESCRIZIONE
    Il certificato di code-signing e' un Certum "Code Signing in the cloud": la chiave
    NON e' esportabile, vive nell'HSM di Certum e si usa via SimplySign Desktop. Non si
    puo' firmare dai runner GitHub usa-e-getta (SimplySign ha solo login grafico), quindi
    si firma in locale: 1 login a SimplySign Desktop (valido ~3h) + questo comando.

    Cosa fa:
      1. scarica gli asset Windows della release (Setup.exe + Portable.zip)
      2. firma il Setup.exe con signtool (timestamp RFC3161 SHA256)
      3. firma l'exe dentro Portable.zip e lo ri-zippa (best-effort)
      4. ri-carica gli asset firmati sulla release (--clobber)
      5. opzionale: se la release e' "draft", la pubblica (-Publish)

    LIMITE NOTO: questo firma cio' che l'utente SCARICA (Setup.exe / Portable). Il
    pacchetto di auto-update (.nupkg) non viene ri-firmato: per quello servirebbe la
    firma durante "vpk pack" (build locale completa). Per SmartScreen sul download
    firmare il Setup.exe e' la cosa che conta.

.PREREQUISITI
    - SimplySign Desktop installato e LOGGATO (ID + OTP). Senza, signtool non trova la chiave.
    - Windows SDK (signtool.exe) — gia' presente con Visual Studio.
    - gh CLI autenticato (gh auth status).

.ESEMPI
    ./firma-release.ps1 -Tag v0.6.64-rc1
    ./firma-release.ps1 -Tag v0.6.64 -Publish      # se la release e' draft, la pubblica dopo la firma
#>
param(
    [Parameter(Mandatory = $true)][string]$Tag,
    [switch]$Publish,
    [string]$Repo = "KonradDallaOrg/dimmy"
)

$ErrorActionPreference = "Stop"
$THUMB = "DD0AD19CFEB75B2D02A58363CA17CF0ED16BFDB4"   # Certum - Konrad Damian Dalla
$TS = "http://time.certum.pl/"

Write-Host ""
Write-Host "==== FIRMA RELEASE $Tag ($Repo) ====" -ForegroundColor Cyan

# --- signtool ---
$signtool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $signtool) { throw "signtool.exe non trovato (Windows SDK mancante)." }

# --- cartella di lavoro ---
$work = Join-Path $env:TEMP ("dimmy-sign-" + ($Tag -replace '[^A-Za-z0-9._-]', '_'))
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory $work | Out-Null

Write-Host "Scarico gli asset Windows..." -ForegroundColor Cyan
gh release download $Tag --repo $Repo --dir $work --pattern "*Setup.exe" --pattern "*Portable.zip"
if ($LASTEXITCODE -ne 0) { throw "gh release download fallito." }

function Invoke-Sign([string]$path) {
    Write-Host "Firmo: $(Split-Path $path -Leaf)" -ForegroundColor Cyan
    & $signtool.FullName sign /sha1 $THUMB /fd sha256 /tr $TS /td sha256 /v "$path"
    if ($LASTEXITCODE -ne 0) {
        throw "Firma fallita su $path. SimplySign Desktop e' aperto e loggato?"
    }
    & $signtool.FullName verify /pa "$path" | Out-Null
}

# --- 1) Setup.exe (download principale) ---
$setup = Get-ChildItem $work -Filter "*Setup.exe" | Select-Object -First 1
if ($setup) { Invoke-Sign $setup.FullName }
else { Write-Host "ATTENZIONE: nessun Setup.exe nella release." -ForegroundColor Yellow }

# --- 2) Portable.zip: firma l'exe interno + ri-zip (best-effort) ---
$zip = Get-ChildItem $work -Filter "*Portable.zip" | Select-Object -First 1
if ($zip) {
    try {
        $ex = Join-Path $work "portable"
        Expand-Archive $zip.FullName -DestinationPath $ex -Force
        $inner = Get-ChildItem $ex -Recurse -Filter "Dimmy.Windows.exe"
        if ($inner) {
            $inner | ForEach-Object { Invoke-Sign $_.FullName }
            Remove-Item $zip.FullName -Force
            Compress-Archive -Path (Join-Path $ex '*') -DestinationPath $zip.FullName -Force
            Write-Host "Portable.zip ri-creato con exe firmato." -ForegroundColor Green
        }
        else { Write-Host "Portable.zip: Dimmy.Windows.exe non trovato, salto." -ForegroundColor Yellow }
    }
    catch {
        Write-Host "Portable.zip: salto (non critico): $($_.Exception.Message)" -ForegroundColor Yellow
    }
}

# --- 3) ricarica firmati ---
$toUpload = @()
if ($setup) { $toUpload += $setup.FullName }
if ($zip -and (Test-Path $zip.FullName)) { $toUpload += $zip.FullName }
if ($toUpload.Count -gt 0) {
    Write-Host "Ricarico gli asset firmati (sostituisco)..." -ForegroundColor Cyan
    gh release upload $Tag @toUpload --repo $Repo --clobber
    if ($LASTEXITCODE -ne 0) { throw "gh release upload fallito." }
}

# --- 4) pubblica se draft ---
if ($Publish) {
    gh release edit $Tag --repo $Repo --draft=false
    Write-Host "Release $Tag pubblicata (da draft)." -ForegroundColor Green
}

Write-Host ""
Write-Host ">>> FATTO: $Tag firmato e ricaricato. <<<" -ForegroundColor Green
