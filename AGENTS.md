# AGENTS.md — Marionette

## Mission
A **Rust OpenAI-compatible proxy pool** with a React + Vite admin dashboard. Built to grow: providers, farms, integrations, and features are added as needed.

Current providers: `grok-cli`, `qoder`, `blackbox`, `freebuff`, plus **BYOK** (user-supplied OpenAI-compatible endpoints).

## Principles (not gates)
- **Secrets never committed**: `.env`, `data/*.sqlite`, token dumps. **Mask tokens** in admin API responses.
- Browser automation lives in Python under `scripts/automation/` — not in the Rust binary.
- Prefer mirroring verified behavior (live-probed upstreams, reference repos in `refs/`) over inventing protocols.
- Dashboard stack: React + Vite + TS SPA. UI follows `docs/DESIGN.md` (LoTM soft, dark-only, English ops nav).

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
