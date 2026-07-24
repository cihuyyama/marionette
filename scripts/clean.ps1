# Clean build artifacts
$ErrorActionPreference = "SilentlyContinue"
Write-Host "[marionette] cleaning build artifacts..." -ForegroundColor Yellow
Remove-Item -Recurse -Force target
Remove-Item -Recurse -Force web\dist
Remove-Item -Recurse -Force web\node_modules
Remove-Item -Force data\*.sqlite
Remove-Item -Force data\*.sqlite-*
Write-Host "[marionette] clean done" -ForegroundColor Green
