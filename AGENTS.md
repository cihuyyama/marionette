# AGENTS.md — Marionette

## Mission
Build a **thin Rust OpenAI-compatible proxy pool** with three providers:
- `grok-cli` (first, complete)
- `qoder` (after dashboard)
- `blackbox` (after qoder; static API keys, farm via temp-mail signup)

Plus a **React + Vite admin dashboard** (after Grok + Admin API), not a full etteeum rewrite.

## Hard constraints
1. **Do not** port all of etteeum-pool (no CodeBuddy/Kiro/Codex/Canva, no full pudidil/compression stack in v1).
2. **Do not** put Playwright / browser automation in Rust v1.
3. **Order locked:** skeleton → Grok CLI full → Admin JSON → React+Vite dashboard → **then** Qoder.
4. Non-stream chat works before stream SSE.
5. Secrets never committed: `.env`, `data/*.sqlite`, token dumps. **Mask tokens** in admin API responses.
6. Prefer mirroring verified behavior from:
   - 9Router grok-cli executor + token refresh
   - etteeum `src/proxy/providers/qoder.ts` (Phase 5 only — do not invent)
   - grok-farm inject format / grok-refresh-quota rules
   - novabox (`refs/novabox`, MIT) for the Blackbox signup/key-harvest flow; our own CF temp-mail worker replaces catchmail.io
   - Blackbox upstream: `api.blackbox.ai/v1/chat/completions` (OpenAI-shaped, Bearer sk-key, no refresh), live-probed
7. Dashboard stack: **React + Vite + TS SPA only** — not TanStack Start / Next / SSR.
8. UI design: follow `docs/DESIGN.md` (LoTM soft, dark-only, English ops nav). Use **Impeccable** + `frontend-ui-ux` when implementing `web/`.

## Implementation order
1. Scaffold Axum + `/health` + `/v1/models` + env + SQLite (`accounts`, `api_keys`) + Provider trait + `grok_cli` stub
2. Grok CLI E2E: import, refresh, non-stream → stream, 429 ~25h cooldown, 401 refresh, 402/403 disable
3. Admin JSON `/admin/*` with `MARIONETTE_ADMIN_KEY` (separate from pool key) + CORS for Vite
4. Dashboard `web/`: scaffold Vite first, then Impeccable craft (Overview, Accounts, Import, Smoke test, Settings)
5. Qoder auth + chat (port from etteeum)
6. Blackbox provider: static `sk-` API keys, `bb/` model prefix, quota kind none, local error classifier (403 = moderation → fallen, never cut), farm = novabox flow ported with our CF temp-mail worker
7. Deploy polish (serve `web/dist`, systemd optional)

## Code style
**Rust**
- Idiomatic Rust 2021
- `tracing` for logs
- `thiserror` for domain errors
- Keep provider trait small and testable
- One provider file each under `src/providers/`

**Dashboard (`web/`)**
- English ops labels only in nav
- Dark-only tokens from `docs/DESIGN.md`
- Soft LoTM on chips / empty / brand — not cosplay nav
- No full OAuth tokens in the browser

## Commands (target)
```bash
cargo run
cargo test
cargo build --release
# later:
# cd web && npm install && npm run dev
# cd web && npm run build
```

## Read first (order)
1. `docs/HANDOFF.md`
2. `docs/ARCHITECTURE.md`
3. `docs/DESIGN.md`
4. `docs/PROVIDER_CHECKLIST.md`
5. This file
