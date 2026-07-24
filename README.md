# Marionette

Rust AI proxy pool for **two providers only**:

1. **grok-cli** — Grok Build / CLI OAuth tokens (from `grok-farm` / 9Router)
2. **qoder** — Qoder PAT / job-token flow (behavior from `etteum-pool`)

Named after *Lord of the Mysteries* marionettes: one controller, many puppet accounts.

> This is **not** a full rewrite of etteeum-pool. Greenfield, thin, 2 providers.

## Status

- [x] Project folder + handoff docs + design brief
- [x] Phase 1 — Cargo/Axum scaffold
- [x] Phase 2 — Grok CLI (code complete; live smoke needs tokens)
- [x] Phase 3 — Admin JSON API
- [x] Phase 4 — React+Vite dashboard (`web/`)
- [x] Phase 5 — Qoder auth + chat (ported from etteeum; live smoke needs tokens)
- [x] Phase 6 — Deploy polish (runbook, static serve, tests)

**Order is intentional:** Grok complete → admin API → dashboard → **then** Qoder.

## How to run

### Quick start (npm scripts)

```powershell
# 1. Copy env and set real keys (never commit .env)
cp .env.example .env

# 2. Install web deps (first time only)
npm run setup

# 3. Dev mode: backend + frontend concurrent (Ctrl+C to stop both)
npm run dev
```

This runs `cargo run` (Axum on `:1940`) + `cd web && npm run dev` (Vite on `:5173`) concurrently.
Vite proxies API calls to Axum automatically.

### All scripts

| Command | What it does |
|---------|-------------|
| `npm run dev` | Run backend + frontend concurrently (dev mode) |
| `npm run dev:api` | Backend only (`cargo run`) |
| `npm run dev:web` | Frontend only (`cd web && npm run dev`) |
| `npm run build` | Build everything: `cargo build --release` + `cd web && npm run build` |
| `npm run build:api` | `cargo build --release` |
| `npm run build:web` | `cd web && npm run build` |
| `npm test` | `cargo test` |
| `npm run import` | Import accounts: `cargo run --bin marionette-import` |
| `npm run setup` | Install web deps: `cd web && npm install` |
| `npm run serve` | Production: `cargo run --release` (serves `web/dist` if `MARIONETTE_STATIC_DIR` set) |
| `npm run clean` | Clean build artifacts (`cargo clean` + `cd web && rm -rf dist`) |

### Env keys (see `.env.example`)

| Variable | Role |
|----------|------|
| `MARIONETTE_HOST` / `MARIONETTE_PORT` | Bind (default `0.0.0.0:1940`) |
| `MARIONETTE_DB` | SQLite path (default `./data/marionette.sqlite`) |
| `MARIONETTE_API_KEY` | Pool chat key (`Authorization: Bearer …` on `/v1/*`) |
| `MARIONETTE_ADMIN_KEY` | Admin API key (separate from pool key) |
| `MARIONETTE_CORS_ORIGIN` | Vite origin (default `http://localhost:5173`) |
| `MARIONETTE_STATIC_DIR` | Serve `web/dist` from Axum (prod, optional) |
| `RUST_LOG` | Tracing filter |

### Import accounts

```bash
# From JSON file
npm run import -- --file path/to/tokens.json --provider grok-cli

# From 9Router SQLite
npm run import -- --from-9router path/to/9router/data.sqlite

# Or directly
cargo run --bin marionette-import -- --file path/to/tokens.json --provider grok-cli
```

### Manual curl

```bash
curl http://127.0.0.1:1940/health
curl -H "Authorization: Bearer $MARIONETTE_API_KEY" http://127.0.0.1:1940/v1/models
curl -H "Authorization: Bearer $MARIONETTE_ADMIN_KEY" http://127.0.0.1:1940/admin/stats
```

Set admin key in dashboard Settings (stored in browser only; never commit tokens).

## Quick context for agents

Read in order:

1. `docs/HANDOFF.md` — full handoff / phases
2. `docs/ARCHITECTURE.md` — system design
3. `docs/DESIGN.md` — dashboard visual brief (LoTM soft)
4. `docs/PROVIDER_CHECKLIST.md` — what to port
5. `AGENTS.md` — coding agent rules

## Related systems (do not confuse)

| Path | Role |
|------|------|
| `C:\Users\miqba\Downloads\gpt\grok-farm` | Create Grok accounts + OAuth tokens (browser) |
| VPS `~/grok-refresh` | Refresh/maintain `grok-cli` tokens in 9Router DB |
| VPS `~/.9router/db/data.sqlite` | 9Router account store |
| `C:\Users\miqba\Downloads\gpt\etteum-pool` | Bun multi-provider pool (reference for Qoder) |

## License / intent

Personal tooling. Keep secrets out of git (tokens, `.env`, DB dumps).
