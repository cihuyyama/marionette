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
- [ ] Phase 5 — Qoder auth + chat (stub only; port from etteeum)
- [ ] Phase 6 — Deploy polish

**Order is intentional:** Grok complete → admin API → dashboard → **then** Qoder.

## How to run

### Backend

```bash
# From repo root
cp .env.example .env   # then set real keys (never commit .env)
cargo run
```

Env keys (see `.env.example`):

| Variable | Role |
|----------|------|
| `MARIONETTE_HOST` / `MARIONETTE_PORT` | Bind (default `0.0.0.0:1940`) |
| `MARIONETTE_DB` | SQLite path (default `./data/marionette.sqlite`) |
| `MARIONETTE_API_KEY` | Pool chat key (`Authorization: Bearer …` on `/v1/*`) |
| `MARIONETTE_ADMIN_KEY` | Admin API key (separate from pool key) |
| `MARIONETTE_CORS_ORIGIN` | Vite origin (default `http://localhost:5173`) |
| `RUST_LOG` | Tracing filter |

```bash
# Health / models
curl http://127.0.0.1:1940/health
curl -H "Authorization: Bearer $MARIONETTE_API_KEY" http://127.0.0.1:1940/v1/models
```

### Import accounts

```bash
cargo run --bin marionette-import -- --file path/to/tokens.json --provider grok-cli
cargo run --bin marionette-import -- --from-9router path/to/9router/data.sqlite
```

### Dashboard

```bash
cd web
npm install
npm run dev
```

Vite proxies to Axum on `:1940`. Set admin key in Settings (stored in browser only; never commit tokens).

### Tests

```bash
cargo test
cargo build --release
```

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
