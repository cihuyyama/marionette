# Marionette Architecture

## High-level

```
Client (OpenCode / Hermes / curl)          Admin UI (React+Vite)
    │  Bearer <pool key>                        │  Bearer <admin key>
    │  model: gcli/*  (qd/* later)              │  /admin/*
    ▼                                           ▼
┌──────────────────────────────────────────────────────────┐
│ Marionette (Rust / Axum)                                 │
│  pool key → chat route by model                          │
│  admin key → account CRUD / stats / import               │
│  pick account → refresh → forward (stream/non-stream)    │
│  optional: serve web/dist static                         │
└──────────────────────┬───────────────────────────────────┘
                       │
               ┌───────┴────────┐
               ▼                ▼
          Grok CLI           Qoder
          (Phase 2)          (Phase 5)
```

**Build order:** Grok E2E → Admin API → Dashboard → Qoder.  
**UI design:** `docs/DESIGN.md` (LoTM soft, dark-only, English ops nav).

## Components

### API layer
- OpenAI-compatible shapes for chat: `/v1/models`, `/v1/chat/completions`
- Admin JSON under `/admin/*` (separate auth key)
- Normalize stream to `text/event-stream` chat.completion.chunk style unless executor requires otherwise
- Map upstream errors to stable JSON errors
- Mask secrets in all admin responses

### Pool
- Account selection: active + not cooling down + matching provider
- Strategies v1: round-robin or random among healthy
- On success: bump `last_used_at`
- On 429: set `cooldown_until = now + 25h`
- On 401: try refresh once, else cooldown
- On 402/403: set inactive (or delete — config flag)

### Providers
Each provider owns:
- auth refresh
- request transform
- response/stream adapt
- error classification

### Admin API (Phase 3 — before dashboard)
Minimum:
- `GET /admin/stats`
- `GET /admin/accounts` (+ filters)
- `GET|PATCH /admin/accounts/:id`
- `POST /admin/accounts/:id/refresh`
- soft-disable / delete
- import (CLI first; HTTP import optional for UI)

Auth: `MARIONETTE_ADMIN_KEY` (distinct from pool `MARIONETTE_API_KEY`).  
CORS: allow Vite dev origin via env.

### Dashboard (Phase 4)
- `web/` React + Vite + TS SPA
- Screens: Overview, Accounts, Import, Smoke test, Settings
- Stack choice: **not** TanStack Start — pure SPA against Admin + pool APIs
- Design system: Impeccable + `docs/DESIGN.md`

### Storage
Own SQLite (`data/marionette.sqlite`), not live-locking 9Router DB.

Import is one-way:
```
9Router / farm JSON / etteeum export  →  marionette.sqlite
```


## Schema (v1 proposal)

```sql
CREATE TABLE accounts (
  id TEXT PRIMARY KEY,
  provider TEXT NOT NULL,          -- grok-cli | qoder
  email TEXT,
  name TEXT,
  is_active INTEGER NOT NULL DEFAULT 1,
  priority INTEGER NOT NULL DEFAULT 0,
  data TEXT NOT NULL,              -- JSON tokens/provider specifics
  cooldown_until TEXT,             -- RFC3339 or NULL
  last_error TEXT,
  last_used_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE api_keys (
  id TEXT PRIMARY KEY,
  key_hash TEXT NOT NULL,
  name TEXT,
  is_active INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL
);

CREATE INDEX idx_accounts_provider_active
  ON accounts(provider, is_active);
```

### `data` JSON examples

**grok-cli**
```json
{
  "accessToken": "...",
  "refreshToken": "...",
  "expiresAt": "2026-07-24T18:00:00.000Z",
  "expiresIn": 21600,
  "clientId": "b1a00492-073a-47ea-816f-4c329264a828",
  "scope": "openid profile email offline_access grok-cli:access api:access",
  "idToken": "..."
}
```

**qoder** (fill after reading etteeum)
```json
{
  "personalToken": "pt-...",
  "accessToken": "jt-...",
  "userId": "...",
  "machineId": "...",
  "refreshToken": "..."
}
```

## Error model

```rust
enum ProviderError {
  AuthExpired,
  AuthInvalid,
  RateLimited { retry_after: Option<Duration> },
  PaymentRequired,
  AccessDenied,
  Upstream { status: u16, body: String },
  Transport(String),
}
```

Pool maps these to account state transitions.

## Concurrency
- Tokio multi-thread OK
- Limit concurrent upstream per account (1) to avoid burning quota
- Global semaphore optional for VPS RAM safety

## Deploy
- Core: single Rust binary + sqlite file + env
- UI: build `web/` → static assets; prefer Axum static serve of `web/dist` (one port) or nginx
- systemd optional later
- VPS RAM-tight: build frontend on dev machine/CI; do not run Vite on VPS
