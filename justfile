# Marionette — justfile
# Install: cargo install just  (or winget install Casey.Just)
# Usage:   just --list

default:
    @just --list

# ── Dev ──────────────────────────────────────────────

# Run backend + frontend concurrently (Ctrl+C stops both)
dev:
    cargo run &
    cd web && npm run dev

# Backend only (Axum on :1940)
dev-backend:
    cargo run

# Frontend only (Vite on :5173)
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
    @test -f .env || cp .env.example .env
    @echo "Setup done. Edit .env to set MARIONETTE_API_KEY and MARIONETTE_ADMIN_KEY."

# ── Clean ────────────────────────────────────────────

# Remove build artifacts (keeps source)
clean:
    cargo clean
    cd web && rm -rf dist node_modules
    @echo "Cleaned target/ + web/dist + web/node_modules"

# ── Production ──────────────────────────────────────

# Run production server (binary + static dist if built)
prod:
    MARIONETTE_STATIC_DIR=./web/dist ./target/release/marionette

# Build then run production
deploy: build
    MARIONETTE_STATIC_DIR=./web/dist ./target/release/marionette

# ── Quick smoke ──────────────────────────────────────

# Quick health check against running server
health:
    curl -s http://127.0.0.1:1940/health

# List models (requires pool key in .env)
models:
    @curl -s -H "Authorization: Bearer $(grep MARIONETTE_API_KEY .env | cut -d= -f2)" http://127.0.0.1:1940/v1/models
