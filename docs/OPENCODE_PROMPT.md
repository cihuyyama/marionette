# Paste into OpenCode

## Bootstrap prompt (Phase 1)

```
Project: Marionette
Path: C:\Users\miqba\Downloads\gpt\marionette

Read first (in order):
1) docs/HANDOFF.md
2) docs/ARCHITECTURE.md
3) docs/DESIGN.md
4) docs/PROVIDER_CHECKLIST.md
5) AGENTS.md

Context summary:
- Rust OpenAI-compatible proxy pool
- Providers: grok-cli first (full), qoder later (Phase 5)
- Name from Lord of the Mysteries (marionette = many controlled accounts)
- NOT a full etteeum rewrite
- Product order LOCKED: skeleton → Grok E2E → Admin JSON → React+Vite dashboard → Qoder
- Dashboard: React+Vite SPA, Impeccable craft, LoTM soft, dark-only, English ops nav
- Grok tokens already in VPS 9Router DB; local farm works; VPS farm often Access denied
- Qoder auth must be ported from etteeum-pool qoder.ts (don't invent) — not yet

Your task now (Phase 1 only):
1. Scaffold Cargo project (axum, tokio, reqwest, sqlx sqlite, serde, tracing, thiserror)
2. Implement /health and /v1/models (gcli/*; qd/* placeholders OK)
3. Env config (MARIONETTE_PORT default 1940)
4. SQLite schema accounts + api_keys
5. Provider trait + grok_cli stub module
6. Keep compiling on Windows

Do not implement Qoder fully yet.
Do not start React dashboard yet (needs Phase 2–3 first).
Do not commit secrets.
```

## After skeleton compiles (Phase 2)

```
Implement Grok CLI provider end-to-end:
- import script from 9Router providerConnections or farm JSON
- ensure_fresh_auth via auth.x.ai refresh
- non-stream chat first, then SSE stream
- 429 cooldown 25h; 401 refresh; 402/403 disable
- pool pick active + not cooling
Use docs/PROVIDER_CHECKLIST.md and 9Router grok-cli executor as source of truth.
Exit gate: real account non-stream + stream via pool key works.
```

## After Grok works (Phase 3)

```
Implement Admin JSON API under /admin/*:
- MARIONETTE_ADMIN_KEY (separate from pool key)
- list/get/patch accounts, stats, refresh, soft-disable
- mask all tokens in responses
- CORS for Vite dev origin
Import may stay CLI-first; add HTTP import if needed for UI.
Do not start full Qoder. Dashboard next after admin API works.
```

## After Admin API works (Phase 4)

```
Build web/ React + Vite + TypeScript dashboard.
Read docs/DESIGN.md and use Impeccable (init/shape/craft) + frontend-ui-ux.
Locks: dark-only, LoTM soft, English nav (Overview, Accounts, Import, Smoke test, Settings).
Screens against live Admin API + pool smoke test.
No TanStack Start. No light mode. No hard LoTM cosplay nav.
Qoder still later (Phase 5).
```
