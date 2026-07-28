# Marionette Admin — Design Pin

Pinned from `docs/DESIGN.md` (2026-07-25). Soft LoTM, dark-only control room.

## Mode
**Operate** — scanability first; brand in chrome, chips, empty states only.

## Nav (exact)
Overview · Accounts · Models · Activity · Setup · Automation · Smoke test · Settings

9Router import lives under **Settings** (not top-level nav).

## Palette
| Token | Hex | Use |
|-------|-----|-----|
| void | `#0B0A0F` | App background |
| ink | `#14121C` | Surfaces / sidebar |
| parchment | `#E8E0D0` | Primary text |
| muted | `#8A8478` | Secondary / labels |
| thread | `#C4A35A` | Accent / Bound / CTA |
| blood | `#8B2E3A` | Danger / Fallen |
| fog | `#4A6B7C` | Sealed / cooldown |
| seal | `#5C4A7A` | Cut / inactive |

## Type
- Display / brand / H1: Instrument Serif
- UI: IBM Plex Sans
- Mono (IDs): IBM Plex Mono

## Status chips
Bound (thread) · Sealed (fog) · Cut (seal) · Fallen (blood) · Channeling (muted)

## Forbidden
Light mode · LoTM nav labels · purple SaaS gradients · full OAuth tokens in UI
