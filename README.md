# Marionette

Rust AI proxy pool for **two providers only**:

1. **grok-cli** — Grok Build / CLI OAuth tokens (from `grok-farm` / 9Router)
2. **qoder** — Qoder PAT / job-token flow (behavior from `etteum-pool`)

Named after *Lord of the Mysteries* marionettes: one controller, many puppet accounts.

> This is **not** a full rewrite of etteeum-pool. Greenfield, thin, 2 providers.

## Status

- [x] Project folder + handoff docs + design brief
- [ ] Phase 1 — Cargo/Axum scaffold
- [ ] Phase 2 — Grok CLI E2E (chat, refresh, 429 cooldown, import)
- [ ] Phase 3 — Admin JSON API
- [ ] Phase 4 — React+Vite dashboard (Impeccable, LoTM soft, dark-only)
- [ ] Phase 5 — Qoder auth + chat
- [ ] Phase 6 — Deploy polish

**Order is intentional:** Grok complete → admin API → dashboard → **then** Qoder.

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
