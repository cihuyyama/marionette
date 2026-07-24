#!/usr/bin/env pwsh
# Build backend + frontend
# Usage: npm run build  OR  pwsh scripts/build.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot

Write-Host "[build] cargo build --release ..." -ForegroundColor Cyan
Push-Location $root
cargo build --release 2>&1 | ForEach-Object { Write-Host $_ }
if ($LASTEXITCODE -ne 0) { Write-Host "[build] cargo FAILED" -ForegroundColor Red; exit 1 }
Pop-Location

Write-Host "[build] npm install (web) ..." -ForegroundColor Cyan
Push-Location "$root\web"
if (-not (Test-Path "node_modules")) { npm install 2>&1 | ForEach-Object { Write-Host $_ } }
Write-Host "[build] npm run build (web) ..." -ForegroundColor Cyan
npm run build 2>&1 | ForEach-Object { Write-Host $_ }
if ($LASTEXITCODE -ne 0) { Write-Host "[build] npm FAILED" -ForegroundColor Red; exit 1 }
Pop-Location

Write-Host "[build] DONE" -ForegroundColor Green
