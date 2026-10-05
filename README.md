# Marionette

**Thin Rust OpenAI-compatible proxy pool** for three providers — plus a dark admin SPA.

| Provider | Model prefix | Auth |
|----------|--------------|------|
| **grok-cli** | `gcli/*`, bare `grok*` | OAuth access + refresh (`auth.x.ai`) |
| **qoder** | `qd/*`, bare `qoder*` | PAT → jobToken / `securityOauthToken` |
| **blackbox** | `bb/*`, bare `blackboxai/*` | static `sk-…` API key (no refresh) |

**One controller, many puppet accounts** — a focused pool, not a multi-provider zoo. No browser automation in the Rust binary.

```
Client (OpenCode / curl)   Bearer pool key  →  /v1/*     →  pool  →  grok-cli | qoder | blackbox
Admin UI / curl            Bearer admin key →  /admin/*
```

| Surface | Default |
|---------|---------|
| API + optional static SPA | `0.0.0.0:1940` |
| Vite dev (proxies API) | `http://localhost:1941` |

---

## Features

- OpenAI-shaped **`POST /v1/chat/completions`** (stream + non-stream) and **`GET /v1/models`**
- Account pool: pick, cooldown, cut/seal, Grok token budget + Qoder credits
- Admin JSON + **React + Vite** dashboard (Overview, Accounts, Models, Activity, Automation, Smoke, Settings)
- Import from JSON / 9Router SQLite / 9Router backup (Settings UI or CLI)
- **Automation** (Python, outside Rust): Qoder Camoufox farm + dudul inject, Grok relogin OAuth farm
- Background Grok token refresh worker

---

## Quick start

Requires [Rust](https://rustup.rs/), [Node 20+](https://nodejs.org/), and [just](https://github.com/casey/just) (`cargo install just` / `winget install Casey.Just`).

```bash
# 1. Env (never commit .env)
cp .env.example .env
# edit MARIONETTE_API_KEY + MARIONETTE_ADMIN_KEY

# 2. Build backend + install web deps
just setup

# 3. Dev: Axum :1940 + Vite :1941 (Ctrl+C stops)
just dev
```

Then open **http://localhost:1941** → Settings → paste admin key (and pool key for Smoke test).

```bash
# Smoke without UI
just health
just models
```

---

## Commands

| Command | What |
|---------|------|
| `just dev` | Backend + frontend (Windows: `scripts/dev-windows.ps1`) |
| `just dev-backend` / `just dev-frontend` | One side only |
| `just build` | Release binary + `web/dist` |
| `just test` / `just preflight` | Tests / build+test |
| `just setup` | First-time build + `npm install` + copy `.env` |
| `just stop` | Kill ports 1940/1941 |
| `just prod` / `just deploy` | Serve release + static SPA |
| `just clean` | `target/` + web dist/node_modules |
| `just health` / `just models` | Quick API smoke |
| `just import-json <file>` | Import account JSON |
| `just import-9router <db>` | Import from 9Router SQLite |
| `just import-9router-backup <file>` | Full backup JSON (grok + qoder) |
| `just import-9router-backup-replace <file>` | Same with wipe |

Without just:

```bash
cargo run --bin marionette
cargo test
cargo build --release
cd web && npm install && npm run dev
cd web && npm run build
```

---

## Models (routing)

| Id examples | Provider |
|-------------|----------|
| `gcli/grok-4.5`, `gcli/grok-build`, `gcli/grok-code-fast-1`, … | grok-cli (max input 256K) |
| `qd/auto`, `qd/ultimate`, `qd/lite`, `qd/qmodel_latest`, `qd/kmodel1`, … | qoder (price · max input · R/VL on **Models**) |

Bare names that start with / contain `grok` → grok-cli; `qoder` / `qd/` → qoder. Full list: `GET /v1/models` or dashboard **Models**.

```bash
curl -s http://127.0.0.1:1940/v1/chat/completions \
  -H "Authorization: Bearer $MARIONETTE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"qd/lite","messages":[{"role":"user","content":"hi"}]}'
```

---

## Environment

See [`.env.example`](.env.example). Names only — put real values in local `.env`.

| Variable | Role | Default |
|----------|------|---------|
| `MARIONETTE_HOST` / `PORT` | Bind | `0.0.0.0` / `1940` |
| `MARIONETTE_DB` | SQLite | `./data/marionette.sqlite` |
| `MARIONETTE_API_KEY` | Pool `/v1/*` | `change-me…` |
| `MARIONETTE_ADMIN_KEY` | Admin `/admin/*` (**separate**) | `change-me-admin…` |
| `MARIONETTE_CORS_ORIGIN` | Vite origin(s) | `http://localhost:1941` |
| `MARIONETTE_STATIC_DIR` | Serve SPA in prod | optional / auto `web/dist` |
| `RUST_LOG` | Tracing | `info,marionette=debug` |
| `MARIONETTE_COOLDOWN_HOURS` | Rate-limit seal | `25` |
| `MARIONETTE_FARM_*` | Automation job runner | see `.env.example` |
| `MARIONETTE_DATA_KEY` | Encrypt `accounts.data` at rest (opt-in, **back it up**) | unset = plaintext |
| `MARIONETTE_LOG_BODY_RETENTION_DAYS` | Expire captured bodies | `7` |
| `MARIONETTE_LOG_RETENTION_DAYS` | Delete request-log rows | `30` |
| `MARIONETTE_RETENTION_INTERVAL_SECS` | Retention sweep interval (`0` disables) | `3600` |
| `QODER_DUDUL_ACCESS_KEY` | Inject only (never commit) | empty |

**Never commit:** `.env`, entire `data/`, farm `accounts*.txt`, farm `results/*.json`, OAuth/PAT dumps.


---

## Log retention

`request_logs` stores captured request/response bodies, which dominate database
size. A background worker keeps it bounded in two stages:

1. **Bodies expire first** (`MARIONETTE_LOG_BODY_RETENTION_DAYS`, default 7) —
   the body columns are nulled, but the metadata row (tokens, credits, status,
   duration) stays queryable.
2. **Rows are deleted later** (`MARIONETTE_LOG_RETENTION_DAYS`, default 30).

Both run in bounded batches so a large backlog never holds the write lock long
enough to stall request logging. Set `MARIONETTE_RETENTION_INTERVAL_SECS=0` to
disable the worker.

## Credentials at rest

By default `accounts.data` (PATs, refresh tokens, API keys) is stored as
plaintext JSON. Set `MARIONETTE_DATA_KEY` to encrypt it with AES-256-CBC — each
value gets a fresh IV and an `enc1:` tag, so the database is opaque without the
key while rows written before encryption still read normally.

> **Warning:** the key is the only way back. Losing it makes every stored
> credential unreadable — there is no recovery path. Back it up outside the
> repo before enabling.


---

## Import accounts

```bash
just import-json path/to/tokens.json
just import-9router path/to/9router/data.sqlite
just import-9router-backup path/to/backup.json
```

Dashboard: **Settings → 9Router import**, or **Accounts → + Add** for single/bulk tokens. Admin API also accepts `POST /admin/accounts` (body up to 32 MiB).

---

## Dashboard

| Nav | Purpose |
|-----|---------|
| Overview | Fleet pulse |
| Accounts | Bound / Sealed / Cut / Fallen, plan chips, inject / warmup |
| Models | Catalog + credit rates |
| Activity | Usage + request log (`day` / `week` / `month` / `all`) |
| Automation | Qoder farm, Grok farm, inject jobs |
| Smoke test | Pool chat probe |
| Settings | Keys (localStorage), 9Router import |

Stack: React 19 + Vite 6 + TS SPA only — dark UI tokens in [`docs/DESIGN.md`](docs/DESIGN.md).

---

## Automation (Python)

Browser work lives under `scripts/automation/` — **not** in the Rust binary.

| Package | Role |
|---------|------|
| [`scripts/automation/qoder_farm`](scripts/automation/qoder_farm/) | GSuite SSO → PAT → optional dudul inject |
| [`scripts/automation/grok_farm`](scripts/automation/grok_farm/) | Relogin + PKCE OAuth for grok-cli |
| [`scripts/automation/blackbox_farm`](scripts/automation/blackbox_farm/) | Signup (CF temp-mail OTP) → `sk-` API key harvest for blackbox |

Start jobs from **Automation** in the UI (needs farm env + Python), or run packages from the CLI. Secrets stay in package-local `.env` / `accounts.txt` (gitignored).

---

## HTTP surface (short)

**Public**

| Method | Path | Auth |
|--------|------|------|
| `GET` | `/health` | none |
| `GET` | `/v1/models` | pool |
| `POST` | `/v1/chat/completions` | pool |
| `POST` | `/v1/messages` | pool (Anthropic Messages) |
| `POST` | `/v1/responses` | pool (OpenAI Responses) |
| `POST` | `/v1/images/generations` | pool |

`/v1/messages` and `/v1/responses` are translation layers: the request folds
into a Chat Completions request and goes through the same pool, so account
selection, failover, quota, and request logging behave identically. Only the
wire shape differs — Anthropic clients (Claude Code) and Responses clients
(Codex) can be pointed at the same pool.

**Admin** (Bearer `MARIONETTE_ADMIN_KEY`): stats, accounts CRUD, import, refresh, usage/requests, providers, inject / warmup / claim-trial, farm jobs.

```bash
curl -s http://127.0.0.1:1940/health
curl -s -H "Authorization: Bearer $MARIONETTE_API_KEY" http://127.0.0.1:1940/v1/models
curl -s -H "Authorization: Bearer $MARIONETTE_ADMIN_KEY" http://127.0.0.1:1940/admin/stats
```

---

## Repo map

```
marionette/
├── src/                 # Axum API, pool, db, providers (grok_cli, qoder), farm runner
├── web/                 # React admin SPA
├── scripts/automation/  # qoder_farm, grok_farm (Camoufox)
├── docs/                # HANDOFF, ARCHITECTURE, DESIGN, PROVIDER_CHECKLIST
├── tests/               # api_smoke + lib unit tests
├── justfile
└── .env.example
```

---

## Docs for agents / deep dive

Read in order:

1. [`AGENTS.md`](AGENTS.md) — hard constraints
2. [`CLAUDE.md`](CLAUDE.md) — dense ops map
3. [`docs/HANDOFF.md`](docs/HANDOFF.md) — phases / history
4. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — design
5. [`docs/DESIGN.md`](docs/DESIGN.md) — UI brief
6. [`docs/PROVIDER_CHECKLIST.md`](docs/PROVIDER_CHECKLIST.md) — provider port notes

---

## Status

| Phase | State |
|-------|--------|
| Skeleton + Grok CLI | done (live smoke needs tokens) |
| Admin JSON | done |
| Dashboard | done |
| Qoder + recovery parity | done (live smoke needs PATs) |
| Farm / inject automation | done (Python + admin UI) |
| Deploy polish | partial (static serve; systemd optional) |

---

## License / intent

Personal ops tooling. **Keep secrets out of git** — tokens, `.env`, `data/`, farm results.
