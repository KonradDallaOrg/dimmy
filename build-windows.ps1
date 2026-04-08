# Dimmy Windows Native UI — Build & Test Script
# Run from PowerShell on Windows:
#   .\build-windows.ps1
#   .\build-windows.ps1 -TestOnly
#   .\build-windows.ps1 -BuildOnly

param(
    [switch]$TestOnly,
    [switch]$BuildOnly,
    [string]$Configuration = "Debug",
    [string]$Platform = "x64"
)

$ErrorActionPreference = "Stop"
$ProjectDir = "$PSScriptRoot\platforms\windows\Dimmy.Windows"
$SolutionFile = "$ProjectDir\Dimmy.Windows.sln"
$TestProject = "$PSScriptRoot\platforms\windows\Dimmy.Windows.Tests\Dimmy.Windows.Tests.csproj"

Write-Host "=== Dimmy Windows Build ===" -ForegroundColor Cyan
Write-Host "Configuration: $Configuration | Platform: $Platform"
Write-Host ""

# Step 1: Restore NuGet packages
if (-not $TestOnly) {
    Write-Host "[1/4] Restoring NuGet packages..." -ForegroundColor Yellow
    dotnet restore $SolutionFile --runtime "win-$Platform" --configfile "$ProjectDir\NuGet.config"
    if ($LASTEXITCODE -ne 0) { Write-Error "Restore failed"; exit 1 }
    Write-Host "  OK" -ForegroundColor Green
}

# Step 2: Build the main project
if (-not $TestOnly) {
    Write-Host "[2/4] Building Dimmy.Windows..." -ForegroundColor Yellow
    dotnet build $SolutionFile -c $Configuration -p:Platform=$Platform --no-restore
    if ($LASTEXITCODE -ne 0) { Write-Error "Build failed"; exit 1 }
    Write-Host "  OK" -ForegroundColor Green
}

# Step 3: Run tests
if (-not $BuildOnly) {
    Write-Host "[3/4] Running tests..." -ForegroundColor Yellow
    dotnet test $TestProject -c $Configuration -p:Platform=$Platform --verbosity normal
    if ($LASTEXITCODE -ne 0) { Write-Error "Tests failed"; exit 1 }
    Write-Host "  OK" -ForegroundColor Green
}

# Step 4: Check Rust tests still pass
if (-not $BuildOnly -and -not $TestOnly) {
    Write-Host "[4/4] Verifying Rust tests..." -ForegroundColor Yellow
    Push-Location "$PSScriptRoot\core"
    cargo test --lib 2>&1
    if ($LASTEXITCODE -ne 0) { Write-Error "Rust tests failed"; Pop-Location; exit 1 }
    Pop-Location
    Write-Host "  OK" -ForegroundColor Green
}

Write-Host ""
Write-Host "=== All checks passed ===" -ForegroundColor Green
