# scripts/setup.ps1 — one-time setup for Marionette
# Installs Rust deps (via cargo build) and web deps (npm install)

$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot/.."

Write-Host "[1/3] Checking Rust..." -ForegroundColor Cyan
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "  Rust not found. Install from https://rustup.rs" -ForegroundColor Red
    exit 1
}

Write-Host "[2/3] Building Rust backend..." -ForegroundColor Cyan
Push-Location $root
cargo build 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { Write-Host "  cargo build failed" -ForegroundColor Red; exit 1 }
Pop-Location

Write-Host "[3/3] Installing web dependencies..." -ForegroundColor Cyan
Push-Location "$root/web"
if (-not (Test-Path node_modules)) {
    npm install 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { Write-Host "  npm install failed" -ForegroundColor Red; exit 1 }
} else {
    Write-Host "  node_modules exists, skipping"
}
Pop-Location

Write-Host ""
Write-Host "Setup complete!" -ForegroundColor Green
Write-Host "  cp .env.example .env  # then set real keys"
Write-Host "  npm run dev           # start backend + frontend"
