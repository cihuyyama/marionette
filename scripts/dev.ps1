# scripts/dev.ps1 — Run backend (cargo) + frontend (vite) concurrently
# Usage: npm run dev  (or: pwsh scripts/dev.ps1)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# Ensure .env exists
if (-not (Test-Path ".env")) {
    if (Test-Path ".env.example") {
        Copy-Item ".env.example" ".env"
        Write-Host "[dev] Created .env from .env.example — edit keys before real use" -ForegroundColor Yellow
    }
}

# Ensure web/node_modules exists
if (-not (Test-Path "web/node_modules")) {
    Write-Host "[dev] Installing web dependencies..." -ForegroundColor Cyan
    Push-Location web; npm install; Pop-Location
}

# Ensure data dir exists
New-Item -ItemType Directory -Path "data" -Force | Out-Null

# Start backend
Write-Host "[dev] Starting Rust backend on :1940..." -ForegroundColor Green
$backend = Start-Process -FilePath "cargo" -ArgumentList "run" -PassThru -NoNewWindow

# Start frontend
Write-Host "[dev] Starting Vite dev server on :5173..." -ForegroundColor Green
$frontend = Start-Process -FilePath "cmd" -ArgumentList "/c", "cd web && npm run dev" -PassThru -NoNewWindow

Write-Host ""
Write-Host "[dev] Marionette running:" -ForegroundColor Cyan
Write-Host "  Backend  : http://127.0.0.1:1940"
Write-Host "  Dashboard: http://localhost:5173"
Write-Host ""
Write-Host "[dev] Press Ctrl+C to stop both..." -ForegroundColor Yellow

try {
    while ($true) {
        if ($backend.HasExited -and -not $frontend.HasExited) {
            Write-Host "[dev] Backend exited, stopping frontend..." -ForegroundColor Red
            Stop-Process -Id $frontend.Id -Force -ErrorAction SilentlyContinue
            break
        }
        if ($frontend.HasExited -and -not $backend.HasExited) {
            Write-Host "[dev] Frontend exited, stopping backend..." -ForegroundColor Red
            Stop-Process -Id $backend.Id -Force -ErrorAction SilentlyContinue
            break
        }
        if ($backend.HasExited -and $frontend.HasExited) {
            Write-Host "[dev] Both exited." -ForegroundColor Red
            break
        }
        Start-Sleep -Seconds 1
    }
} finally {
    if (-not $backend.HasExited) { Stop-Process -Id $backend.Id -Force -ErrorAction SilentlyContinue }
    if (-not $frontend.HasExited) { Stop-Process -Id $frontend.Id -Force -ErrorAction SilentlyContinue }
}
