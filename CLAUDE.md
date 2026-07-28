# CLAUDE.md — Marionette

**For:** Claude Code CLI and coding agents working in this repo.  
**Generated from source** (commit `1f5eae2` lineage). Dense ops guide — not a product essay.  
**Also read:** `AGENTS.md`, `docs/HANDOFF.md`, `docs/ARCHITECTURE.md`, `docs/DESIGN.md`, `docs/PROVIDER_CHECKLIST.md`.

---

## 1. What this is

Thin **Rust** OpenAI-compatible **proxy pool** with exactly **two** providers:

| Provider ID | Model prefix | Auth |
|-------------|--------------|------|
| `grok-cli` | `gcli/*`, bare `grok*` | OAuth access + refresh (`auth.x.ai`) |
| `qoder` | `qd/*`, bare `qoder*` | PAT → jobToken / `securityOauthToken` + userId/machineId |

Plus a **React + Vite + TypeScript** admin SPA under `web/` (dark-only, LoTM soft).

**Not** a full etteeum-pool rewrite. No multi-provider zoo. No Playwright/browser automation in Rust v1.

Name: *Lord of the Mysteries* marionettes — one controller, many puppet accounts.

---

## 2. Golden rules (hard constraints)

1. **Only** `grok-cli` and `qoder`. Do not add CodeBuddy/Kiro/Codex/Canva/etc.
2. **Do not** port all of etteeum (no full pudidil/compression stack in v1).
3. **Do not** put Playwright / browser automation in the Rust binary.
4. **Secrets never committed:** `.env`, entire `data/` (sqlite, token dumps, proxy lists), `.omo/`.
5. **Mask tokens** in every admin JSON response (`db::mask_token` / `mask_secrets`).
6. **Grok vs Qoder error policy is different:**
   - Global `classify_http_status` still maps HTTP codes; **pool effects** live in `apply_provider_error`.
   - Grok **402 / PaymentRequired** (spending-limit / fleet credit) → **sealed** cooldown + `quota_remaining=0` (not cut). Auto-restores quota when cooldown ends.
   - Grok **403 / AccessDenied** and **AuthInvalid** (`invalid_grant`) → **cut**.
   - Qoder uses **local** `classify_qoder_status`: 402/403 → `RateLimited` (cooldown), **not** cut.
   - **Never change global `classify_http_status` to “fix” Qoder** — keep Qoder classification local.
7. Dashboard: **React + Vite SPA only** — not Next, TanStack Start, or SSR.
8. UI: `docs/DESIGN.md` — dark-only, English ops nav, soft LoTM on chips/empty/brand only.
9. Prefer mirroring verified behavior from 9Router grok-cli + etteeum `qoder.ts` — do not invent Qoder auth.

---

## 3. Repo map

```
marionette/
├── Cargo.toml              # bins: marionette, marionette-import; lib: marionette
├── justfile                # dev/build/test/import/prod
├── .env.example            # env names only
├── AGENTS.md               # short agent rules
├── CLAUDE.md               # this file
├── src/
│   ├── main.rs             # Axum serve, CORS, optional static web/dist, refresh worker spawn
│   ├── lib.rs              # module exports
│   ├── config.rs           # Config::from_env
│   ├── state.rs            # AppState { pool, config, http, grok, qoder }
│   ├── auth.rs             # PoolAuth, AdminAuth (Bearer extractors)
│   ├── error.rs            # AppError, ProviderError
│   ├── openai.rs           # ChatCompletionRequest, default_models(), provider_id()
│   ├── pool.rs             # handle_chat, apply_provider_error, should_retry_same_account
│   ├── db.rs               # SQLite schema, pick_account, mask_*, quotas, request_logs
│   ├── import_util.rs      # 9Router backup / connection JSON → Account
│   ├── bin/import.rs       # CLI marionette-import
│   ├── api/
│   │   ├── mod.rs          # router (public + admin)
│   │   ├── health.rs       # GET /health
│   │   ├── models.rs       # GET /v1/models, GET /admin/models
│   │   ├── chat.rs         # POST /v1/chat/completions → pool::handle_chat
│   │   └── admin.rs        # stats, accounts CRUD, import, refresh, usage, providers
│   ├── providers/
│   │   ├── mod.rs          # Provider trait, ChatOutcome, classify_http_status, force_refresh default
│   │   ├── grok_cli.rs     # OAuth refresh + cli-chat-proxy.grok.com
│   │   ├── qoder.rs        # COSY crypto, jobToken, stream/non-stream (~2k LOC)
│   │   └── qoder-baseprompt.json
│   └── workers/
│       ├── mod.rs
│       └── refresh.rs      # background Grok token refresh (not Qoder)
├── tests/api_smoke.rs      # router oneshot + temp sqlite
├── web/                    # React admin SPA
│   ├── src/App.tsx         # routes
│   ├── src/lib/api.ts      # admin/pool fetch helpers
│   ├── src/lib/settings.ts # localStorage keys (no OAuth secrets)
│   ├── src/pages/*         # Overview, Accounts, Import, Smoke, …
│   └── vite.config.ts      # :1941 proxy → :1940
├── docs/                   # HANDOFF, ARCHITECTURE, DESIGN, PROVIDER_CHECKLIST
└── scripts/                # e.g. dev-windows.ps1
```

**Hot files (LOC order):** `qoder.rs` ≫ `db.rs` > `AccountList.tsx` > `grok_cli.rs` > `pool.rs` > `admin.rs`.

---

## 4. Ports & runtime topology

| Surface | Port / path |
|---------|-------------|
| Axum API + optional static | `MARIONETTE_HOST:PORT` default `0.0.0.0:1940` |
| Vite dev | `1941` — proxies `/admin`, `/v1`, `/health` → `127.0.0.1:1940` |
| Prod static | `MARIONETTE_STATIC_DIR` or auto `web/dist` if present |

```
Client (OpenCode/curl)  Bearer pool key  →  /v1/*  →  pool  →  grok-cli | qoder
Admin UI / curl         Bearer admin key →  /admin/*
```

---

## 5. Commands

Prefer **just** (`cargo install just` / `winget install Casey.Just`).

| Recipe | What |
|--------|------|
| `just setup` | `cargo build` + `web npm install` + copy `.env` if missing |
| `just dev` | Backend + frontend (Windows: `scripts/dev-windows.ps1`) |
| `just dev-backend` | `cargo run --bin marionette` |
| `just dev-frontend` | `cd web && npm run dev` |
| `just build` | release binary + `web` dist |
| `just test` | `cargo test` |
| `just preflight` | build + test |
| `just import-json <file>` | CLI import JSON |
| `just import-9router <db>` | import from 9Router sqlite |
| `just import-9router-backup <file>` | full backup JSON (grok+qoder filter) |
| `just import-9router-backup-replace <file>` | same with wipe |
| `just health` / `just models` | smoke against running server |
| `just prod` / `just deploy` | run release + static dir |
| `just stop` | kill ports 1940/1941 (platform-specific) |
| `just clean` | target + web dist/node_modules |

Fallback:

```bash
cargo run --bin marionette
cargo test
cargo build --release
cargo run --bin marionette-import -- --file path.json
cd web && npm install && npm run dev
cd web && npm run build   # tsc --noEmit && vite build
```

---

## 6. Environment (names only — never commit values)

From `.env.example` / `Config::from_env`:

| Variable | Role | Default |
|----------|------|---------|
| `MARIONETTE_HOST` | bind host | `0.0.0.0` |
| `MARIONETTE_PORT` | bind port | `1940` |
| `MARIONETTE_DB` | sqlite path | `./data/marionette.sqlite` |
| `MARIONETTE_API_KEY` | pool chat key | `change-me` |
| `MARIONETTE_ADMIN_KEY` | admin API key (**separate**) | `change-me-admin` |
| `MARIONETTE_CORS_ORIGIN` | Vite origin(s), comma-ok | `http://localhost:1941` |
| `RUST_LOG` | tracing filter | `info,marionette=debug` |
| `MARIONETTE_COOLDOWN_HOURS` | RateLimited cooldown | `25` |
| `MARIONETTE_GROK_CLIENT_ID` | OAuth client | `b1a00492-073a-47ea-816f-4c329264a828` |
| `MARIONETTE_REFRESH_LEAD_SECS` | refresh before expiry | `10800` |
| `MARIONETTE_REFRESH_INTERVAL_SECS` | worker loop; `0` disables | `1800` |
| `MARIONETTE_REFRESH_WORKERS` | concurrent refresh | `8` |
| `MARIONETTE_STATIC_DIR` | serve SPA | optional / auto `web/dist` |

`.gitignore` ignores **entire `data/`**, `.env`, `.omo/`, `refs/`.

---

## 7. Auth model

| Extractor | Header | Accepts | Routes |
|-----------|--------|---------|--------|
| `PoolAuth` | `Authorization: Bearer …` | `MARIONETTE_API_KEY` **or** active row in `api_keys` (SHA-256 hex of key) | `/v1/*` |
| `AdminAuth` | same | **only** `MARIONETTE_ADMIN_KEY` | `/admin/*` |
| none | — | — | `/health` |

Dashboard (`web/src/lib/settings.ts`):

- localStorage key: `marionette.admin.settings.v1`
- fields: `baseUrl`, `adminKey`, `poolKey`, `adminKeyExpiresAt` (admin key cleared after ~24h)
- **Never** store full OAuth/SOT/PAT in the browser beyond what admin API already masks
- Dev: empty/`127.0.0.1:1940` base → relative URLs so Vite proxy works

---

## 8. HTTP API (exact from `src/api/mod.rs`)

### Public

| Method | Path | Auth |
|--------|------|------|
| GET | `/health` | none |
| GET | `/v1/models` | pool |
| POST | `/v1/chat/completions` | pool |

### Admin

| Method | Path | Auth |
|--------|------|------|
| GET | `/admin/stats` | admin |
| GET | `/admin/connection` | admin |
| GET | `/admin/models` | admin |
| GET | `/admin/usage` | admin |
| GET | `/admin/requests` | admin |
| GET | `/admin/providers` | admin |
| PATCH | `/admin/providers/{provider}` | admin |
| GET | `/admin/accounts` | admin (query: provider, status) |
| POST | `/admin/accounts` | admin (import; body raw JSON bytes; `?replace=true`) |
| GET | `/admin/accounts/{id}` | admin |
| PATCH | `/admin/accounts/{id}` | admin |
| DELETE | `/admin/accounts/{id}` | admin |
| POST | `/admin/accounts/{id}/refresh` | admin |

Body limit: **32 MiB** (`main.rs` `DefaultBodyLimit`) for large 9Router backups.

Error JSON shape:

```json
{ "error": { "message": "…", "type": "marionette_error", "code": 401 } }
```

---

## 9. Models & routing

`ChatCompletionRequest::provider_id()` (`openai.rs`):

- starts with `gcli/` **or** `grok` **or** contains `grok` → `"grok-cli"`
- starts with `qd/` **or** `qoder` → `"qoder"`
- else → unknown model (400)

`upstream_model()`: strip first `prefix/` if present.

`default_models()` lists (non-exhaustive copy — confirm in `openai.rs` if editing):

**Grok:** `gcli/grok-build`, `gcli/grok-4.5`, `-high/-medium/-low`, `gcli/grok-4`, `gcli/grok-4-fast-reasoning`, `gcli/grok-code-fast-1`, `gcli/grok-3`

**Qoder:** `qd/auto`, `qd/ultimate`, `qd/performance`, `qd/efficient`, `qd/lite`, `qd/qmodel_preview` (Qwen3.8-Max-Preview), `qd/qmodel_latest`, `qd/qmodel1`, `qd/kmodel_latest` (Kimi-K3), `qd/kmodel1` (Kimi-K2.7-Code), `qd/gm51model1` (GLM-5.2), `qd/dmodel1`, `qd/dfmodel1`, `qd/mmodel` (MiniMax-M3) — one listed id per live upstream; legacy aliases (`qmodel`, `kmodel`, `gm51model`, …) still route in `model_cfg`

OpenAI request also passes through optional `tools` / `tool_choice` / `parallel_tool_calls` and message `tool_calls` / `tool_call_id`.

---

## 10. Pool flow (`src/pool.rs::handle_chat`)

1. Resolve `provider_id` from model; select `Arc<dyn Provider>` (`grok` or `qoder`).
2. Loop **up to 8** picks: `db::pick_account(pool, provider_id, &tried)`.
3. Push account id to `tried`.
4. `provider.ensure_fresh_auth(&mut account)` — on fail → `apply_provider_error` + next account.
5. Persist account; `provider.chat(&account, &req)`.
6. **On `Ok`:** `handle_chat_success` (usage log, quota decrement for grok token budget, stream usage oneshot).
7. **On `Err`:** if `should_retry_same_account(provider_id, &e, false)`  
   - true only when `provider_id == "qoder"` **and** `AuthExpired` **and** not already retried  
   - then `force_refresh` → persist → **one** more `chat` **without** new `tried` entry and **without** burning an outer attempt slot  
   - if retry fails or refresh fails → `apply_provider_error` + continue  
   - else → `apply_provider_error` + continue  
8. After loop: last error or `NoAccounts`.

### `apply_provider_error` matrix

| ProviderError | Account effect |
|---------------|----------------|
| `RateLimited { … }` | `cooldown_until = now + MARIONETTE_COOLDOWN_HOURS` (or retry_after) — **sealed** |
| `PaymentRequired` / `Upstream` **402** | **sealed** (same cooldown hours) + `quota_remaining=0` (Grok credit/spending-limit recovers) |
| `AuthExpired` | first: 5 min cooldown; if last_error already auth-ish: **cut** (`is_active=0`) |
| `AuthInvalid` / `AccessDenied` | **cut** |
| `Upstream` status **403** | **cut** |
| other | record `last_error` only |

### Status labels (`Account::status_label`)

| Label | Meaning |
|-------|---------|
| `bound` | active, not cooling, quota ok, **no** `last_error` |
| `sealed` | cooldown or quota exhausted (takes priority over fallen) |
| `cut` | `is_active=0` (takes priority; hard auth death / 403) |
| `fallen` | active + not sealed, but **any** residual `last_error` (unclassified upstream/transport/other). Cleared on next successful chat. Error text always stored in `last_error` and shown in admin UI. |

LoTM UI chips map these (Bound/Sealed/Cut/Fallen) — see DESIGN.md.

---

## 11. Provider trait (`src/providers/mod.rs`)

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError>;
    async fn chat(&self, account: &Account, req: &ChatCompletionRequest)
        -> Result<ChatOutcome, ProviderError>;
    async fn force_refresh(&self, account: &mut Account) -> Result<(), ProviderError> {
        self.ensure_fresh_auth(account).await  // default
    }
}
```

`ChatOutcome::Json(Value)` | `ChatOutcome::Stream { response, usage_rx }`.

### Global classifier (Grok path — do not casually edit)

```text
401 → AuthExpired
402 → PaymentRequired
403 → AccessDenied
429 → RateLimited
body hints invalid_grant → AuthInvalid; rate/quota → RateLimited
else Upstream
```

### Qoder-local (`classify_qoder_status` in `qoder.rs`)

```text
403 | 402 → RateLimited   // temporary exhaust
401       → AuthExpired
else      → classify_http_status(...)
```

`force_refresh` (Qoder override): clear `securityOauthToken` + `userId`, re-run jobToken exchange (`apply_job_token`), write `account.data`.

### Grok (`grok_cli.rs`)

- Refresh: `POST https://auth.x.ai/oauth2/token` (`grant_type=refresh_token`)
- Chat upstream: `https://cli-chat-proxy.grok.com/v1/responses` (OpenAI-shaped adaptation)
- Background worker: `workers/refresh.rs` (Grok only; interval/lead/workers from env)

### Qoder (`qoder.rs`)

- Faithful COSY port: AES-128-CBC + RSA, jobToken, machineId stable
- Chat clients: HTTP/2 preferred, HTTP/1.1 fallback (reqwest built **with http2**, **without** compress codecs — avoids SSE UnexpectedEof)
- Import copies numeric `expireTime` (psd then top-level) for lazy refresh
- **KNOWN LIMITATION (P0 accepted):** mid-stream SSE 403 **after** `Ok(Stream)` returned to client stays client-only `io::Error` frame — does **not** trigger pool cooldown. Documented in code; candidate P1.

---

## 12. Data model (`db.rs` migrate)

Tables:

| Table | Purpose |
|-------|---------|
| `accounts` | id, provider, email, name, is_active, priority, data (JSON), cooldown_until, last_error, last_used_at, timestamps, **quota_limit**, **quota_remaining** |
| `api_keys` | hashed pool keys |
| `request_logs` | activity / usage / credits |
| `provider_settings` | load_balance strategy, sticky, rr_cursor |

**Grok quota:** `GROK_TOKEN_QUOTA = 1_000_000` tokens per account (kind `tokens`). Qoder: no token budget (`none` / RPM elsewhere).

**Account `data` JSON (examples):**

- grok-cli: `accessToken`, `refreshToken`, `expiresAt`, `expiresIn`, `clientId`, `idToken`, …
- qoder: `personalToken`, `securityOauthToken` / access job token, `userId`, `machineId`, `expireTime`, …

Always use `Account::data_json()` / `set_data_json` for new token JSON I/O.

**Mask keys** (admin public views):  
`accessToken`, `refreshToken`, `idToken`, `personalToken`, `securityOauthToken`, `machineToken`, snake_case variants.  
`mask_token`: `abcd…wxyz` (or `****` if ≤8 chars).

---

## 13. Import

### CLI (`marionette-import`)

```
--file <path.json> [--provider grok-cli|qoder] [--replace] [--db path]
--from-9router <data.sqlite> [...]
--from-9router-backup <backup.json> [--replace]
```

### HTTP

`POST /admin/accounts` with raw JSON body (file upload from UI uses `importAccountsFile`).  
`?replace=true` wipes supported providers first.

### Rules

- 9Router backup auto-filters to grok-cli + qoder.
- Qoder requires `personalToken`; copies `expireTime` when present.
- **No** mass invalidation of SOT on import (avoids thundering herd).

UI split:

- **+ Add** modal per provider (`AddAccountModal`) — single/bulk tokens
- **Settings → 9Router import** — backup file only (`NineRouterImport`; `/import` redirects to `/settings`)

---

## 14. Dashboard (`web/`)

| Item | Value |
|------|--------|
| Stack | React 19, Vite 6, TS, react-router-dom 7 — **no** component library |
| Routes | `/` Overview, `/accounts`, `/accounts/:provider`, `/models`, `/activity`, `/setup`, `/automation`, `/smoke`, `/settings` (`/import` → Settings) |
| Nav (English ops) | Overview · Accounts · Models · Activity · Setup · Automation · Smoke test · Settings |
| Design | `docs/DESIGN.md` / `web/DESIGN.md` — void/ink/parchment/thread gold; dark only |
| API client | `web/src/lib/api.ts` — `auth: "admin" \| "pool" \| "none"` |
| Auth gate | `AuthGate` + `marionette-unauthorized` event on 401 |

When changing UI: load **impeccable** + frontend craft; keep nav English; no full tokens in browser.

---

## 15. Testing

### Integration (`tests/api_smoke.rs`)

Temp sqlite + `api::router(state)` + `tower::ServiceExt::oneshot`:

- `s1_health_ok`
- `s2_models_with_pool_key` / `s3_models_requires_key`
- `s4_admin_stats_ok` / `s5_admin_wrong_key`
- `mask_token_unit`
- `provider_routing`

### Unit (`cargo test --lib`)

- `pool::tests::*` — `should_retry_same_account` matrix
- `providers::qoder::tests::*` — classify 401/402/403/500 + parse service error
- `import_util::tests::*` — grok/qoder parse, expireTime, backup filter

**TDD rule for behavior changes (pool/errors/import):** write failing test first → smallest green → `cargo test` full suite. Do not delete failing tests to “pass”.

Gate before claim-done: `cargo test` + preferably `just preflight`.

---

## 16. Gotchas

1. **reqwest:** `http2` feature required for Qoder SSE; **no** compression codecs (project comment).
2. **Do not edit** global `classify_http_status` for Qoder 403 — use `classify_qoder_status` only inside qoder paths.
3. **Qoder mid-stream 403** after stream starts: no pool cooldown (P0 limitation).
4. **Grok refresh worker** only; Qoder refresh is on-demand / force_refresh on AuthExpired.
5. **Windows:** PowerShell; `curl.exe`; line endings LF→CRLF warnings are usually cosmetic.
6. **Own DB:** never live-lock 9Router sqlite; one-way import only.
7. **Body size:** large backups need the 32 MiB limit + raw body import path.

---

## 17. Coding conventions

**Rust**

- Edition 2021, idiomatic
- `tracing` for logs; never log full tokens
- `thiserror` for `AppError` / `ProviderError`
- One provider file under `src/providers/`
- Keep `Provider` trait small and testable
- Release: LTO + `codegen-units = 1`

**TypeScript**

- Strict SPA; match existing `api.ts` / settings patterns
- English ops labels in nav
- Design tokens from DESIGN.md

**Git**

- Do not commit `.env`, `data/**`, secrets
- Atomic commits preferred (feat/fix/chore scope like existing log: `feat(qoder):`, `fix(qoder):`, `feat(web):`)

---

## 18. Related systems (read-only reference)

| Path / host | Role |
|-------------|------|
| `…/grok-farm` | Farm Grok OAuth tokens |
| `…/etteum-pool` | Bun multi-provider; **Qoder reference** `src/proxy/providers/qoder.ts` |
| VPS 9Router DB | token store to import from — not runtime dependency |
| VPS `grok-refresh` | hygiene for 9Router grok rows |

Do not modify those repos unless the user explicitly asks.

---

## 19. Do / Don't

| Do | Don't |
|----|--------|
| Read HANDOFF → ARCHITECTURE → DESIGN → this file | Invent Qoder auth |
| Mask secrets in admin responses | Commit tokens / `.env` / `data/` dumps |
| Use Qoder-local 402/403 → cooldown | Map Qoder 402/403 via global classifier to cut Grok-style |
| Same-account force_refresh once on Qoder AuthExpired | Infinite refresh loops / multi-retry storms |
| `cargo test` before claiming done | Delete tests to green |
| React+Vite SPA + DESIGN.md | Next/SSR or light mode v1 |
| Port from etteeum/9Router when unsure | “Simplify” crypto/stream paths |

---

## 20. Quick agent checklist (new task)

1. Identify surface: pool / provider / admin / import / web.
2. Confirm provider error policy (Grok global vs Qoder local).
3. Add/adjust unit test if behavior changes.
4. `cargo test` (+ `cd web && npm run build` if UI).
5. Manual smoke if API: `GET /health`, `GET /v1/models` with pool key.
6. No secrets in diff.

---

## 21. Phase status (high level)

| Phase | Status |
|-------|--------|
| 1 Skeleton | done |
| 2 Grok CLI E2E (code) | done; live smoke needs tokens |
| 3 Admin JSON | done |
| 4 Dashboard | done (polish optional) |
| 5 Qoder port | code done; live smoke needs PATs |
| 5.5 Qoder P0 recovery parity | done (force_refresh, classify_qoder, expireTime, same-account retry) |
| 6 Deploy polish | partial (static serve exists; systemd optional) |

Details: `docs/HANDOFF.md`.

---

**End of CLAUDE.md.** Prefer linking to `docs/*` for narrative history; keep this file operational.
