# Marionette

Rust AI proxy pool for **two providers only**:

1. **grok-cli** â€” Grok Build / CLI OAuth tokens (from `grok-farm` / 9Router)
2. **qoder** â€” Qoder PAT / job-token flow (behavior from `etteum-pool`)

Named after *Lord of the Mysteries* marionettes: one controller, many puppet accounts.

> This is **not** a full rewrite of etteeum-pool. Greenfield, thin, 2 providers.

## Status

- [x] Project folder + handoff docs + design brief
- [x] Phase 1 â€” Cargo/Axum scaffold
- [x] Phase 2 â€” Grok CLI (code complete; live smoke needs tokens)
- [x] Phase 3 â€” Admin JSON API
- [x] Phase 4 â€” React+Vite dashboard (`web/`)
- [x] Phase 5 â€” Qoder auth + chat (ported from etteeum; live smoke needs tokens)
- [x] Phase 6 â€” Deploy polish (runbook, static serve, tests)

**Order is intentional:** Grok complete â†’ admin API â†’ dashboard â†’ **then** Qoder.

## How to run

Requires [just](https://github.com/casey/just) (`cargo install just` or `winget install Casey.Just`).

### Quick start

```bash
# 1. Copy env and set real keys (never commit .env)
cp .env.example .env

# 2. First-time setup: build backend + install web deps
just setup

# 3. Dev mode: backend + frontend concurrent (Ctrl+C stops both)
just dev
```

`just dev` runs `cargo run` (Axum on `:1940`) + `cd web && npm run dev` (Vite on `:1941`) concurrently.
Vite proxies API calls to Axum automatically.

### All commands

| Command | What it does |
|---------|-------------|
| `just dev` | Run backend + frontend concurrently (dev mode) |
| `just dev-backend` | Backend only (`cargo run`) |
| `just dev-frontend` | Frontend only (`cd web && npm run dev`) |
| `just build` | Build everything: `cargo build --release` + `cd web && npm run build` |
| `just test` | `cargo test` |
| `just preflight` | Build + test (run before deploy) |
| `just setup` | First-time: build backend + `cd web && npm install` + copy `.env` |
| `just clean` | Remove `target/` + `web/dist` + `web/node_modules` |
| `just health` | Curl `/health` against running server |
| `just models` | List models (requires pool key in `.env`) |
| `just import-json <file>` | Import accounts from JSON |
| `just import-9router <db>` | Import from 9Router SQLite |

### Env keys (see `.env.example`)

| Variable | Role |
|----------|------|
| `MARIONETTE_HOST` / `MARIONETTE_PORT` | Bind (default `0.0.0.0:1940`) |
| `MARIONETTE_DB` | SQLite path (default `./data/marionette.sqlite`) |
| `MARIONETTE_API_KEY` | Pool chat key (`Authorization: Bearer â€¦` on `/v1/*`) |
| `MARIONETTE_ADMIN_KEY` | Admin API key (separate from pool key) |
| `MARIONETTE_CORS_ORIGIN` | Vite origin (default `http://localhost:1941`) |
| `MARIONETTE_STATIC_DIR` | Serve `web/dist` from Axum (prod, optional) |
| `RUST_LOG` | Tracing filter |

### Import accounts

```bash
# From JSON file
just import-json path/to/tokens.json

# From 9Router SQLite
just import-9router path/to/9router/data.sqlite
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

1. `docs/HANDOFF.md` â€” full handoff / phases
2. `docs/ARCHITECTURE.md` â€” system design
3. `docs/DESIGN.md` â€” dashboard visual brief (LoTM soft)
4. `docs/PROVIDER_CHECKLIST.md` â€” what to port
5. `AGENTS.md` â€” coding agent rules

## Related systems (do not confuse)

| Path | Role |
|------|------|
| `C:\Users\miqba\Downloads\gpt\grok-farm` | Create Grok accounts + OAuth tokens (browser) |
| VPS `~/grok-refresh` | Refresh/maintain `grok-cli` tokens in 9Router DB |
| VPS `~/.9router/db/data.sqlite` | 9Router account store |
| `C:\Users\miqba\Downloads\gpt\etteum-pool` | Bun multi-provider pool (reference for Qoder) |

## License / intent

Personal tooling. Keep secrets out of git (tokens, `.env`, DB dumps).
