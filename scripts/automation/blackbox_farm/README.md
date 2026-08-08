# blackbox_farm

Python package for **Blackbox.ai signup + API-key harvest** inside Marionette.

**Path:** `scripts/automation/blackbox_farm`
**Not** part of the Rust proxy binary. Browser automation stays here only.
Browser flow ported 1:1 from `refs/novabox` (MIT, verified against live
app.blackbox.ai). Temp-mail is OUR self-hosted `cloudflare_temp_email` worker —
never catchmail.io.

## Scope (v1)

| Does | Does not |
|------|----------|
| Signup on app.blackbox.ai (Playwright Chromium, Next.js server action) | Write to 9Router SQLite |
| OTP via cloudflare temp-mail worker (6 digits from BODY only) | IMAP / gmail modes |
| Create API key, capture `sk-...` from POST /api/v0/keys or page scan | Camoufox / anti-detect (plain chromium like novabox) |
| Validate key with a live 8-token chat completion | Playwright in Rust |
| 9Router-shaped JSON for `marionette-import` | Proactive scheduler (runner-driven) |

## Pipeline

```
register:COUNT:domain  (accounts.txt)
  -> CF temp-mail: POST /admin/new_address  (address + jwt)
  -> Chromium: /signup fill email+password -> submit (Next.js server action)
  -> poll /api/parsed_mails for 6-digit OTP (body only, never subject)
  -> fill OTP -> Verify -> land on /activity
  -> /keys -> CREATE KEY -> name (random company name) -> CREATE API KEY
  -> capture key from POST /api/v0/keys response body (fallback page scan)
  -> close DONE modal -> validate key (ministral-3b "Say OK")
  -> DELETE temp-mail address, close browser
  -> results/blackbox-accounts.json  (providerConnections, provider=blackbox)
  -> NDJSON account_ok event -> Marionette auto-import (src/farm.rs)
```

## Setup

```powershell
cd scripts/automation
# blackbox_farm uses system-wide playwright (no venv needed if already installed)
python -m playwright install chromium
copy blackbox_farm\.env.example blackbox_farm\.env
copy blackbox_farm\accounts.txt.example blackbox_farm\accounts.txt
# edit blackbox_farm\.env -> BLACKBOX_CF_MAIL_* (or rely on DB mail settings via runner)
```

## Env

| Variable | Role | Default |
|----------|------|---------|
| `BLACKBOX_HEADLESS` | Headless chromium | `true` |
| `BLACKBOX_TIMEOUT` | Per-step browser timeout (s) | `30` |
| `BLACKBOX_OTP_TIMEOUT` | OTP mailbox poll budget (s) | `120` |
| `BLACKBOX_CF_MAIL_BASE_URL` | cloudflare_temp_email worker base URL | — |
| `BLACKBOX_CF_MAIL_ADMIN_PASSWORD` | `x-admin-auth` admin password | — |
| `BLACKBOX_CF_MAIL_DOMAIN` | catch-all mailbox domain | — |
| `BLACKBOX_CF_MAIL_SITE_PASSWORD` | `x-custom-auth` (only if worker in private mode) | — |
| `BLACKBOX_ACCOUNT_TIMEOUT` | Wall-clock budget per account (s) | `600` |
| `BLACKBOX_OUTPUT` | Output JSON path | `results/blackbox-accounts.json` |
| `BLACKBOX_SCREENSHOT_DIR` | Failure screenshots | `screenshots` |

Runner note: `src/farm.rs` injects the `BLACKBOX_CF_MAIL_*` values from the DB
mail settings (`/admin/mail-settings`) on every register job, so a configured
dashboard overrides the package `.env`.

## Run

`PYTHONPATH` = parent of the package (`scripts/automation`):

```powershell
cd scripts\automation
$env:PYTHONPATH = (Get-Location).Path
python -m blackbox_farm -f blackbox_farm\accounts.txt --json-progress --concurrency 2 --no-headless
```

### Flags

| Flag | Meaning |
|------|---------|
| `-f accounts.txt` | register directive `register:COUNT:domain` |
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
- Independent of `grok_farm` / `qoder_farm` (duplicated helpers on purpose).
