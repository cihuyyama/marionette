# blackbox_farm

Python package for **Blackbox.ai account farming** inside Marionette.

**Path:** `scripts/automation/blackbox_farm`
**Not** part of the Rust proxy binary. Browser automation stays here only.
Browser flow ported 1:1 from `refs/novabox` (MIT, verified against live
app.blackbox.ai), then hardened (Camoufox + humanize engine). Temp-mail is
OUR self-hosted `cloudflare_temp_email` worker — never catchmail.io.

## Two modes

| Mode | Input | Flow |
|------|-------|------|
| `register` | `register:COUNT:domain` | signup new accounts (temp-mail OTP) → key |
| `google-sso` | `email\|password` lines | login **existing** GSuite/Google accounts via "Continue with Google" → key (no OTP, no temp-mail) |

GSuite mode exists because the temp-mail OTP flow is flaky in practice
(first verification email rarely arrives; resend hits 429 cooldowns and
intermittent 500s). A GSuite login is direct: email + Google password only.

## Scope

| Does | Does not |
|------|----------|
| Signup / Google-SSO login on app.blackbox.ai (Camoufox + humanize) | Write to 9Router SQLite |
| OTP via cloudflare temp-mail worker (register mode only) | IMAP / gmail modes |
| Create API key, capture `sk-...` from POST /api/v0/keys or page scan | Playwright in Rust |
| Validate key with a live 8-token chat completion | Proactive scheduler (runner-driven) |
| 9Router-shaped JSON for `marionette-import` | |

## Pipeline

### register

```
register:COUNT:domain  (accounts.txt)
  -> CF temp-mail: POST /admin/new_address  (address + jwt)
  -> Camoufox: /signup fill email+password -> submit (Next.js server action)
  -> poll /api/parsed_mails for 6-digit OTP (body only), resend loop with
     parsed 429 cooldown + 500 backoff
  -> fill OTP -> Verify -> land on /activity
  -> /keys -> CREATE KEY -> name (random company name) -> CREATE API KEY
     (retried 4x; capture from POST /api/v0/keys or page scan)
  -> validate key (ministral-3b "Say OK")
  -> DELETE temp-mail address, close browser
  -> results/blackbox-accounts.json  (providerConnections, provider=blackbox)
  -> NDJSON account_ok event -> Marionette auto-import (src/farm.rs)
```

### google-sso

```
email|password lines  (accounts.txt)
  -> Camoufox: /login -> "Continue with Google"
     (popup-aware: drives the OAuth popup if one opens)
  -> Google auth dance (email -> password -> consent / gaplustos /
     Workspace welcome), driver ported from qoder_farm/google_sso.py
  -> land back on app.blackbox.ai
  -> same CREATE KEY harvest -> validate -> export + account_ok
```

## Setup

```powershell
cd scripts/automation
python -m camoufox fetch
copy blackbox_farm\.env.example blackbox_farm\.env
copy blackbox_farm\accounts.txt.example blackbox_farm\accounts.txt
# register mode: edit .env -> BLACKBOX_CF_MAIL_* (or rely on DB mail settings via runner)
# google-sso mode: no mail config needed
```

## Env

| Variable | Role | Default |
|----------|------|---------|
| `BLACKBOX_HEADLESS` | Headless browser | `true` |
| `BLACKBOX_TIMEOUT` | Per-step browser timeout (s) | `30` |
| `BLACKBOX_OTP_TIMEOUT` | OTP poll budget incl. resend loop (s) | `240` |
| `BLACKBOX_CF_MAIL_BASE_URL` | cloudflare_temp_email worker base URL | — |
| `BLACKBOX_CF_MAIL_ADMIN_PASSWORD` | `x-admin-auth` admin password | — |
| `BLACKBOX_CF_MAIL_DOMAIN` | catch-all mailbox domain | — |
| `BLACKBOX_CF_MAIL_SITE_PASSWORD` | `x-custom-auth` (private mode only) | — |
| `BLACKBOX_ACCOUNT_TIMEOUT` | Wall-clock budget per account (s) | `600` |
| `BLACKBOX_OUTPUT` | Output JSON path | `results/blackbox-accounts.json` |
| `BLACKBOX_SCREENSHOT_DIR` | Failure screenshots | `screenshots` |
| `BLACKBOX_HUMANIZE` | Camoufox humanized mouse | `true` |
| `BLACKBOX_BROWSER_OS` | Camoufox spoofed OS | `windows` |

Runner note: `src/farm.rs` injects the `BLACKBOX_CF_MAIL_*` values from the DB
mail settings (`/admin/mail-settings`) on every register job, so a configured
dashboard overrides the package `.env`.

## Run

`PYTHONPATH` = parent of the package (`scripts/automation`):

```powershell
cd scripts\automation
$env:PYTHONPATH = (Get-Location).Path
# register mode
python -m blackbox_farm -f blackbox_farm\accounts.txt --json-progress --concurrency 2 --no-headless
# google-sso mode (accounts.txt = email|password lines)
python -m blackbox_farm -f blackbox_farm\accounts.txt --json-progress --no-headless
```

Dashboard: **Automation → Blackbox** starts register jobs; google-sso jobs can
be started via `POST /admin/farm/start` with `email|password` lines as
`accounts` (farm.rs passes them through unchanged).

### Flags

| Flag | Meaning |
|------|---------|
| `-f accounts.txt` | `register:COUNT:domain` directive OR `email\|password` lines |
| `-o out.json` | 9Router-shaped output |
| `--concurrency N` | parallel browsers |
| `--headless` / `--no-headless` | browser mode (overrides `BLACKBOX_HEADLESS`) |
| `--account-retries N` | full-pipeline retries per account (default 1) |
| `--account-delay S` | delay/stagger between accounts (only if > 0) |
| `--json-progress` | NDJSON events for dashboard |
| `--debug` | verbose + screenshots on error |

Exit code: `0` if fail == 0, else `1`.

## NDJSON progress schema (`--json-progress`)

```json
{"type":"farm","provider":"blackbox","ts":"2026-08-08T10:00:00.000Z","level":"STEP","msg":"signup - signing up...","email":"abc@dom","step":"signup","ok":0,"fail":0,"total":1,"elapsed_s":12.3}
{"type":"farm","provider":"blackbox","event":"account_ok","ts":"2026-08-08T10:01:00.000Z","level":"OK","msg":"account ready for import","email":"abc@dom","email_masked":"a***c@dom","step":"import","ok":1,"fail":0,"total":1,"elapsed_s":62.1,"path":".../blackbox-accounts.json"}
{"type":"farm","provider":"blackbox","event":"finished","ts":"2026-08-08T10:01:01.000Z","ok":1,"fail":0,"total":1,"elapsed_s":63.0}
```

## Import into Marionette

```bash
just import-json scripts/automation/blackbox_farm/results/blackbox-accounts.json
# or
cargo run --bin marionette-import -- --file scripts/automation/blackbox_farm/results/blackbox-accounts.json
```

## Export JSON shape

```json
{
  "providerConnections": [
    {
      "id": "…",
      "provider": "blackbox",
      "email": "user@temp.example",
      "name": "user@temp.example",
      "displayName": "user@temp.example",
      "isActive": true,
      "priority": 0,
      "createdAt": "2026-…Z",
      "updatedAt": "2026-…Z",
      "apiKey": "sk-…",
      "password": "N…!a7#…",
      "farmMeta": { "farm": "blackbox-farm", "farmedAt": "2026-…Z" }
    }
  ],
  "exportedAt": "…",
  "source": "marionette/scripts/automation/blackbox_farm",
  "count": 1
}
```

`password` is kept on purpose: it is needed to log back in and recreate keys
later. `marionette-import` ignores unknown fields.

## Notes

- Secrets: never commit `.env`, `accounts.txt`, `results/`, `screenshots/`, `data/`.
- OTP extraction scans message BODY only — message-id timestamps in subjects
  false-positive on `\d{6}`.
- Abort guard: 4 consecutive failures stop the remaining backlog.
- google-sso mode keeps the Google password in the export (same rationale as
  the register password — key re-creation later).
- Independent of `grok_farm` / `qoder_farm` (duplicated helpers on purpose).
