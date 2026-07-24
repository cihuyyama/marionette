# Marionette — OpenCode Handoff

**Created:** 2026-07-25  
**Owner:** Iqbal (Windows local + VPS)  
**Goal:** Greenfield Rust proxy pool for **Grok CLI + Qoder only**.  
**Name origin:** Lord of the Mysteries — *marionette* (many controlled accounts).

This file is the single source of truth when continuing in **OpenCode** or a new Hermes session.

---

## 1. What to build

OpenAI-compatible proxy:

```
GET  /health
GET  /v1/models
POST /v1/chat/completions   # stream + non-stream
Authorization: Bearer <pool-api-key>
```

Providers:
| ID | Upstream idea | Auth |
|----|---------------|------|
| `grok-cli` | Grok Build CLI path (`cli-chat-proxy.grok.com` style, same tokens as 9Router) | OAuth access + refresh |
| `qoder` | Qoder chat (same behavior as etteeum Qoder provider) | PAT → job/session token + machineId/userId |

**Out of scope v1:** full etteeum rewrite, multi-provider zoo, React dashboard, browser login bots, pudidil/compression suite.

---

## 2. Why this exists

- etteeum-pool = Bun + Hono + many providers (heavy, TS-fast iteration)
- User wants **Rust**, but only **2 providers** → rewrite scope is viable
- Grok tokens already farmed + stored in 9Router
- Qoder behavior already proven in etteeum; port carefully

Decision trail:
- Full etteeum → Rust: overkill
- Go + TanStack: good alternative
- **Rust + 2 providers only: approved direction**
- Product name: **marionette** (not qogrok / etteeum-rs)

---

## 3. Local & VPS map

### Local Windows
| Item | Path |
|------|------|
| This project | `C:\Users\miqba\Downloads\gpt\marionette` |
| Grok farm | `C:\Users\miqba\Downloads\gpt\grok-farm` |
| etteeum-pool | `C:\Users\miqba\Downloads\gpt\etteum-pool` |
| Farm success export | `grok-farm\results\_import_local_ok.json` (18 accounts snapshot) |
| Import helper | `grok-farm\import_local_to_9router.py` |

### VPS `43.156.232.106` (ubuntu)
SSH:
```bash
ssh -i C:\Users\miqba\Documents\tencent_lighthouse.pem ubuntu@43.156.232.106
```

| Item | Path / note |
|------|-------------|
| 9Router source | `/home/ubuntu/9router_wyx0` |
| 9Router DB | `/home/ubuntu/.9router/db/data.sqlite` |
| 9Router port | `20128` (public via domain reverse proxy) |
| Grok farm | `/home/ubuntu/grok-farm` |
| Grok hygiene | `/home/ubuntu/grok-refresh` (`grok-refresh` + `grok-quota` systemd) |
| WARP for farm only | **WarpProxy SOCKS5 `127.0.0.1:40000`** — NOT full tunnel |
| Farm proxies.txt | `socks5://127.0.0.1:40000` |

**Important farm note (2026-07-24):**  
Local farm OAuth often works; VPS farm often hits **Access denied** at consent even via WARP. User may buy residential proxy later. Marionette does **not** depend on VPS farming — it consumes tokens already in DB/export.

---

## 4. Token sources to import

### Grok CLI (priority #1)
9Router table `providerConnections`:
- `provider = "grok-cli"`
- `authType = "oauth"`
- `data` JSON fields:
  - `accessToken`
  - `refreshToken`
  - `expiresAt`
  - `expiresIn` (often 21600)
  - `scope`
  - `clientId` = `b1a00492-073a-47ea-816f-4c329264a828`
  - `idToken` optional
  - error/cooldown fields may exist: `lastError`, `errorCode`, `backoffLevel`

As of last import (2026-07-24): **1018** grok-cli rows after adding **18** local farm successes (`bibib.biz.id`).

Refresh rules (mirror `grok-refresh` / quota pack):
- Refresh via `POST https://auth.x.ai/oauth2/token`
  - `grant_type=refresh_token`
  - `client_id=b1a00492-073a-47ea-816f-4c329264a828`
  - `refresh_token=...`
- `invalid_grant` → delete/disable account
- **429 free usage** → cooldown ~**25 hours** (rolling, not strict midnight)
- **401** → cooldown / refresh attempt
- **402 / 403** → disable or delete

Optional also store `grok-web` (SSO cookie) later — **not required for v1**.

### Qoder (priority #2)
Reference implementation:
- `C:\Users\miqba\Downloads\gpt\etteum-pool\src\proxy\providers\qoder.ts`

Critical quirks (from ops skill):
- Stored `personalToken` (`pt-...`) is **not** always bare chat Bearer
- Chat goes through refresh/jobToken (`jt-...`) + `userId` / `machineId`
- Bare userinfo on raw `pt-` can 401 while chat still works
- Model example: `qd-Lite` / lite may need static model config fallback in some stacks

**Do not invent Qoder auth.** Read etteeum code first, then port.

---

## 5. Suggested architecture

```
marionette/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── api/{mod,chat,models,health,admin}.rs
│   ├── pool/{mod,cooldown}.rs
│   ├── providers/{mod,grok_cli,qoder}.rs
│   ├── auth/{grok_refresh,qoder_auth}.rs
│   ├── db.rs
│   └── error.rs
├── data/                 # gitignored sqlite
├── scripts/              # import from 9router / farm json
├── web/                  # React + Vite + TS admin dashboard
│   ├── PRODUCT.md        # Impeccable product context
│   ├── DESIGN.md         # visual world (LoTM soft, dark-only)
│   └── src/
└── docs/
    └── DESIGN.md         # same design brief (source of truth until web/ exists)
```

### Stack
**Backend**
- Rust 2021
- **axum** + tokio
- **reqwest** (streaming)
- **sqlx** + SQLite
- serde / serde_json
- tracing
- thiserror

**Dashboard (after Grok E2E)**
- React + Vite + TypeScript (SPA, **not** TanStack Start / Next)
- Impeccable skill for design system + craft
- Dark-only; LoTM identity soft (see `docs/DESIGN.md`)
- Dev: Vite `:5173` proxies to Axum `:1940`
- Prod: Axum serves `web/dist` (preferred) or reverse-proxy static

### Provider trait (sketch)
```rust
#[async_trait]
trait Provider {
    fn id(&self) -> &'static str;
    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<()>;
    async fn chat(&self, account: &Account, req: ChatRequest)
        -> Result<ChatOutcome>; // json or sse stream
}
```

### Pool flow
1. Authenticate client with pool API key
2. Map model → provider (`gcli/*` or `qd/*` — decide convention early)
3. Pick active account not in cooldown
4. `ensure_fresh_auth`
5. Call upstream
6. Update account state (success / cooldown / disable)

### Default env (proposal)
```env
MARIONETTE_HOST=0.0.0.0
MARIONETTE_PORT=1940
MARIONETTE_DB=./data/marionette.sqlite
MARIONETTE_API_KEY=change-me
MARIONETTE_ADMIN_KEY=change-me-admin
MARIONETTE_CORS_ORIGIN=http://localhost:5173
RUST_LOG=info,marionette=debug
```

Port **1940** avoids clash with etteeum `1930/1931` and 9router `20128`.

Admin auth: **separate** `MARIONETTE_ADMIN_KEY` (not the same as pool chat key).

---

## 6. Implementation phases

**Locked order (2026-07-25 brainstorm):**  
Grok full → Admin API → React+Vite dashboard (Impeccable + LoTM soft) → **then** Qoder.

### Phase 0 — done
- [x] Name locked: marionette
- [x] Folder + handoff docs
- [x] Plan: dashboard before Qoder; design brief `docs/DESIGN.md`

### Phase 1 — skeleton (Rust) — done
- [x] `cargo init`
- [x] Axum server
- [x] `/health`, `/v1/models` (hardcoded `gcli/*`; `qd/*` placeholders OK, chat may 501)
- [x] Config from env
- [x] SQLite migrations for `accounts`, `api_keys`
- [x] Provider trait + `grok_cli` stub
- [x] Compile on Windows

### Phase 2 — Grok CLI E2E — code complete (live smoke needs tokens)
- [x] Import script from 9Router SQLite or farm JSON (`marionette-import`)
- [x] Non-stream chat completion
- [x] Stream SSE OpenAI-compatible
- [x] Token refresh (`auth.x.ai`) + persist rotated refresh
- [x] 429 cooldown ~25h; 401 refresh; 402/403 disable
- [x] Pool pick: active + not cooling
- [ ] Smoke test with ≥1 real account (non-stream + stream) — needs live tokens

**Exit gate:** curl with pool key + `gcli/grok-4.5` works end-to-end (code path ready; run when tokens available).

### Phase 3 — Admin JSON API — done
- [x] `GET /admin/accounts` (filter provider/status; **mask tokens**)
- [x] `GET /admin/accounts/:id`
- [x] `PATCH /admin/accounts/:id` (active, priority, clear cooldown)
- [x] `POST /admin/accounts/:id/refresh`
- [x] `DELETE` or soft-disable account
- [x] `GET /admin/stats`
- [x] Import: CLI primary; optional `POST /admin/accounts/import` for UI
- [x] Admin key auth + CORS for Vite dev origin
- [x] Never return full OAuth secrets in JSON

### Phase 4 — Dashboard (React + Vite) — done (`web/`)
Use skill **impeccable** + `frontend-ui-ux`; category `visual-engineering`.

- [x] **4a** Impeccable init/shape → `web/PRODUCT.md`, pin `docs/DESIGN.md` / `web/DESIGN.md`
- [x] **4b** Vite + React + TS scaffold; design tokens (dark-only LoTM soft)
- [x] **4c** App shell (sidebar English ops labels)
- [x] **4d** Screens: Overview, Accounts, Import, Smoke test, Settings
- [ ] **4e** Polish / a11y / visual-qa (optional follow-up)

**Nav labels (English ops only):**  
`Overview` · `Accounts` · `Import` · `Smoke test` · `Settings`

**Design locks:** see `docs/DESIGN.md`  
- Soft LoTM (chips/empty/brand only; not cosplay nav)  
- Dark only  
- Operate mode (dense admin tool first)

### Phase 5 — Qoder (stub only; not ported)
- [ ] Port auth from etteeum `qoder.ts` (do not invent)
- [ ] Non-stream then stream
- [ ] Import accounts
- [ ] Model aliases (`qd/lite`, etc.)
- [ ] Dashboard: provider filter + smoke models for qoder

`src/providers/qoder.rs` returns Phase 5 not-implemented errors until full port.

### Phase 6 — Deploy polish
- [ ] Serve `web/dist` from Axum (or nginx)
- [ ] systemd unit for VPS
- [ ] README run / import / UI (partial: README how-to-run added)

---

## 7. Reference code locations

### Must-read for Grok
- 9Router (VPS): `9router_wyx0/open-sse/providers/registry/grok-cli.js` (or similar)
- 9Router token refresh: `open-sse/services/tokenRefresh*` / xai refresh
- Hermes skill: `9router-instance-operations` → `references/grok-providers.md`, `grok-refresh-quota-pack.md`
- Farm inject format: `grok-farm/farm.py` → `auto_inject_to_9router`

### Must-read for Qoder
- `etteum-pool/src/proxy/providers/qoder.ts`
- Hermes skill: `etteum-pool-operations` (+ Qoder notes)
- 9Router Qoder import notes: `9router-instance-operations` → `references/qoder-etteum-token-import.md`

### Do not start from
- Full etteeum router/compression as mandatory v1
- VPS WARP full-tunnel (breaks other apps; farm uses SOCKS5 only)

---

## 8. Model naming convention (decide in scaffold)

Recommended:

```
gcli/grok-4.5
gcli/grok-4.5-high
qd/lite
qd/Lite
```

`/v1/models` should list both families. Client uses one pool base URL.

---

## 9. Security

- DB has live OAuth refresh tokens — treat as secrets
- Never log full access/refresh tokens
- Backup before any bulk DB write
- Gitignore: `.env`, `data/*.sqlite`, `*.json` token dumps

---

## 10. Acceptance criteria

### Usable product (Grok + dashboard) — primary track
1. `cargo build --release` succeeds on Windows (and ideally Linux VPS)
2. Import ≥1 grok-cli account and complete one chat (non-stream)
3. Stream chat works for same account
4. Expired access token auto-refreshes via refresh_token
5. Simulated/real 429 marks cooldown, account skipped by pool
6. Admin API lists accounts with **masked** tokens; stats work
7. Dashboard: view accounts, toggle active, import, smoke test chat
8. UI matches `docs/DESIGN.md` (dark-only, soft LoTM, English ops nav)
9. README has run / import / UI instructions

### Full two-provider (after Phase 5)
10. ≥1 Qoder account chat works
11. Dashboard supports qoder filter + models

---

## 11. Open questions (resolve while coding)

1. Exact Grok CLI upstream base URL + headers (copy from 9Router executor — verify live)
2. Whether to support only chat completions or also `/v1/responses` later
3. Single SQLite file vs import-on-start from 9Router path (recommend **own DB + import**, don't lock 9Router file)
4. Qoder model list minimal set for Phase 5
5. Admin import: UI upload only vs CLI + UI (recommend **CLI in Phase 2, UI in Phase 4**)

---

## 12. First OpenCode prompt (copy-paste)

```
Continue project Marionette at C:\Users\miqba\Downloads\gpt\marionette

Read in order:
docs/HANDOFF.md, docs/ARCHITECTURE.md, docs/DESIGN.md, docs/PROVIDER_CHECKLIST.md, AGENTS.md

Scaffold a Rust Axum app (Phase 1 only):
- /health
- /v1/models (hardcode gcli/*; qd/* placeholders OK)
- env MARIONETTE_PORT=1940
- SQLite schema accounts + api_keys
- Provider trait + grok_cli stub

Do NOT implement Qoder fully.
Do NOT start React dashboard until Phase 2–3 done (see HANDOFF order).
Keep secrets out of git. Follow AGENTS.md.
```

After Phase 2–3, dashboard work must use Impeccable + `docs/DESIGN.md`.

---

## 13. User preferences (relevant)

- Prefers short Indonesian replies in chat; technical docs can be EN
- VPS is RAM-tight (~2GB): concurrent browsers bad; Rust binary good
- Already runs 9Router + grok-refresh; Marionette should coexist
- Farm concurrent=1; proxy for VPS farm is separate issue
- **Product order:** Grok complete → admin API → React+Vite dashboard → Qoder later
- **UI:** React+Vite (not TanStack Start); Impeccable craft; LoTM soft; dark only; English ops labels

---

**End of handoff.** Phases 1–4 code-complete as of 2026-07-25; next is live Grok smoke (tokens) then Phase 5 Qoder.
