# Marionette — justfile
# Install: cargo install just  (or winget install Casey.Just)
# Usage:   just --list

default:
    @just --list

# ── Dev ──────────────────────────────────────────────

# Run backend + frontend (Windows: same window; Unix: concurrent)
# Waits until :1940 accepts connections so Vite proxy does not spam ECONNREFUSED.
[windows]
dev:
    @echo "Starting backend (wait until :1940 is up), then frontend..."
    @powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev-windows.ps1
    @echo "Done."

[unix]
dev:
    @echo "Starting backend (wait until :1940 is up), then frontend..."
    @echo "Backend:  http://127.0.0.1:1940"
    @echo "Frontend: http://localhost:1941"
    cargo run --bin marionette &
    @i=0; while [ $$i -lt 150 ]; do if (echo >/dev/tcp/127.0.0.1/1940) >/dev/null 2>&1; then echo "Backend ready."; break; fi; i=$$((i+1)); sleep 0.4; done
    cd web && npm run dev

# Backend only (Axum on :1940)
dev-backend:
    cargo run --bin marionette

# Frontend only (Vite on :1941)
dev-frontend:
    cd web && npm run dev

# ── Build ────────────────────────────────────────────

# Build everything: Rust release + web dist
build: build-backend build-frontend

build-backend:
    cargo build --release

build-frontend:
    cd web && npm run build

# ── Test ─────────────────────────────────────────────

# Run Rust tests
test:
    cargo test

# Build + test (preflight before deploy)
preflight: build test

# ── Import ───────────────────────────────────────────

# Import from JSON file: just import-json path/to/tokens.json
import-json file:
    cargo run --bin marionette-import -- --file {{file}}

# Import from 9Router SQLite: just import-9router path/to/data.sqlite
import-9router db:
    cargo run --bin marionette-import -- --from-9router {{db}}

# ── Setup ────────────────────────────────────────────

# First-time setup: build backend, install web deps, copy .env
setup:
    cargo build
    cd web && npm install
    @powershell -NoProfile -Command "if (-not (Test-Path .env)) { Copy-Item .env.example .env }"
    @echo Setup done. Edit .env to set MARIONETTE_API_KEY and MARIONETTE_ADMIN_KEY.

# ── Clean ────────────────────────────────────────────

# Remove build artifacts (keeps source)
clean:
    cargo clean
    cd web && powershell -NoProfile -Command "Remove-Item -Recurse -Force dist,node_modules -ErrorAction SilentlyContinue"
    @echo Cleaned target/ + web/dist + web/node_modules

# Stop any running backend or frontend processes on port 1940 and 1941
[windows]
stop:
    @echo Stopping dev processes on ports 1940 and 1941...
    -@powershell -NoProfile -Command 'Stop-Process -Name "marionette" -Force -ErrorAction SilentlyContinue; $p = Get-NetTCPConnection -LocalPort 1940,1941 -ErrorAction SilentlyContinue; if ($p) { $p | ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue } }; exit 0'
    @echo Done.

[unix]
stop:
    @echo "Stopping dev processes..."
    -pkill -f "marionette" || true
    -fuser -k 1940/tcp || true
    -fuser -k 1941/tcp || true
    @echo "Done."

# ── Production ──────────────────────────────────────

# Run production server (binary + static dist if built)
[windows]
prod:
    @powershell -NoProfile -Command '$env:MARIONETTE_STATIC_DIR="./web/dist"; ./target/release/marionette.exe'

[unix]
prod:
    MARIONETTE_STATIC_DIR=./web/dist ./target/release/marionette

# Build then run production
deploy: build
    just prod

# ── Quick smoke ──────────────────────────────────────

# Quick health check against running server
health:
    curl.exe -s http://127.0.0.1:1940/health

# List models (requires pool key in .env)
models:
    @powershell -NoProfile -Command "$key = (Select-String -Path .env -Pattern 'MARIONETTE_API_KEY=' | ForEach-Object { $_.Line.Split('=',2)[1] }); curl.exe -s -H \"Authorization: Bearer $key\" http://127.0.0.1:1940/v1/models"
