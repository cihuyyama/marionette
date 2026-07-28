# Marionette Design Brief

**Status:** locked for Phase 4 dashboard (2026-07-25)  
**Skill:** Impeccable (`init` / `shape` / craft) + `frontend-ui-ux`  
**Mode:** **Operate** — admin tool first; brand in chrome and empty states, not cosplay.

This is the visual source of truth until `web/DESIGN.md` is generated/copied during Phase 4a.

---

## Product one-liner

Control room for a Rust AI proxy pool: many Grok (then Qoder) accounts as marionettes, one operator.

Named after *Lord of the Mysteries* marionettes — **soft** thematic identity only.

---

## Locked decisions

| Decision | Value |
|----------|--------|
| Framework | React + Vite + TypeScript SPA |
| Not using | TanStack Start, Next, Remix, full SSR |
| Theme | **Dark only** (no light mode v1) |
| Lore intensity | **Soft** |
| Nav / ops labels | **English only** |
| Density | Dense tables, scanable status, low decoration noise |

### Nav (exact labels)

```
Overview
Accounts
Models
Activity
Setup
Automation
Smoke test
Settings
```

9Router import lives under **Settings** (not top-level nav).

Do **not** rename nav to Spirit Vision / Marionettes / Whisper / etc.

### Soft LoTM — where flavor is allowed

| Surface | Allowed |
|---------|---------|
| Product name / logo | **Marionette** |
| Status chips | Bound · Sealed · Cut · Fallen · Channeling |
| Empty states | Short flavor line + clear CTA |
| Optional page subtitle | e.g. Accounts → `Bound marionettes` (secondary, small) |
| Visual motifs | Thread gold lines, seal-like chips, restrained ink surfaces |
| Tooltips | Always show technical truth (`cooldown_until`, `is_active`) |

### Soft LoTM — where flavor is forbidden

- Sidebar primary labels (must stay English ops)
- Form field names and error codes (keep technical)
- Table column headers (Email, Provider, Status, Cooldown, Last used, Error)
- Full-screen tarot / novel artwork as chrome
- Gothic fonts for body/UI text
- Neon cyberpunk / Matrix green / generic purple SaaS gradient

---

## Visual world

**Metaphor:** Church-of-the-Fool **control room** — one operator, many threads.  
**Feel:** Victorian occult **restrained** — ink, parchment, bronze gold, fog. Not anime splash.

### Palette

| Token | Hex | Use |
|-------|-----|-----|
| `void` | `#0B0A0F` | App background |
| `ink` | `#14121C` | Surfaces / cards / sidebar |
| `parchment` | `#E8E0D0` | Primary text |
| `muted` | `#8A8478` | Secondary text, labels |
| `thread` | `#C4A35A` | Accent, CTA, Bound / active |
| `blood` | `#8B2E3A` | Danger, Fallen, hard errors |
| `fog` | `#4A6B7C` | Sealed / cooldown / waiting |
| `seal` | `#5C4A7A` | Cut / inactive |

Borders: low-contrast warm gray on `ink`, not pure white lines.  
Focus rings: `thread` at reduced opacity.  
No pure `#FFFFFF` body text.

### Typography

| Role | Direction |
|------|-----------|
| Display / brand / page H1 | Elegant serif (e.g. Instrument Serif, Cormorant) |
| UI body / tables / forms | Readable sans (e.g. IBM Plex Sans, Source Sans 3) |
| IDs / tokens / mono | IBM Plex Mono or JetBrains Mono |

**Ban:** blackletter / unreadable gothic for table or form text.

### Status → chip mapping

| Pool state | Chip label | Color token |
|------------|------------|-------------|
| active + healthy | Bound | `thread` |
| cooldown | Sealed | `fog` |
| inactive / disabled | Cut | `seal` |
| dead auth / invalid_grant | Fallen | `blood` |
| refresh in flight | Channeling | `muted` + thin spinner |

Tooltip / detail panel always exposes raw fields.

### Motifs (use sparingly)

1. **Thread** — 1px gold hairline in sidebar edge or header under-logo  
2. **Seal chips** — rounded status pills (not loud badges everywhere)  
3. **Empty state** — single quiet illustration or line-art silhouette; one flavor sentence  
4. **Motion** — micro only (refresh pulse, soft seal appear); no confetti, no parallax noise  

### Layout

- Left sidebar + main content (desktop-first; usable tablet)
- Overview: stat cards (total / Bound / Sealed / Cut / Fallen) + recent errors  
- Accounts: filterable dense table; row actions (toggle, refresh, open detail)  
- Import: paste JSON or file upload → admin import API  
- Smoke test: model + message → non-stream pool chat; show response + which account used if API provides it  
- Settings: base URL + admin key in **localStorage only** (never commit)

### Copy examples (soft)

| Context | Copy |
|---------|------|
| Empty accounts | No marionettes bound yet. Import from 9Router or farm JSON. |
| Cooldown chip | Sealed |
| Cooldown tooltip | Cooldown until 2026-07-26T12:00:00Z |
| Disable action | Disable (or “Cut thread” only if button secondary label) |
| Refresh action | Refresh auth |

Prefer English ops on buttons; optional flavor only if it stays unambiguous.

---

## Screens (Phase 4 scope)

1. **Overview** — counts + health snapshot  
2. **Accounts** — list / filter / detail drawer  
3. **Import** — bind accounts into pool  
4. **Smoke test** — probe pool chat  
5. **Settings** — connection to Marionette API  

Out of scope v1 UI: full chat client, multi-user login, light theme, Qoder-specific forms (generic provider fields OK).

---

## Impeccable workflow (Phase 4)

1. `impeccable` **init** → `web/PRODUCT.md`  
2. **shape** / new-work → pin this brief into `web/DESIGN.md`  
3. Implement shell + tokens first  
4. Screens against live Admin API (Phase 3 gate)  
5. `polish` / `audit` / visual-qa before “dashboard done”  

When implementing UI agents must load: `impeccable`, `frontend-ui-ux`.

---

## Anti-goals

- Generic shadcn defaults with zero token customization  
- TanStack Start / SSR stack for a private admin SPA  
- Hard LoTM cosplay that hurts scanability  
- Displaying full access/refresh tokens in the browser  

---

## Related docs

- Phases & order: `docs/HANDOFF.md` §6  
- System design: `docs/ARCHITECTURE.md`  
- Agent rules: `AGENTS.md`
