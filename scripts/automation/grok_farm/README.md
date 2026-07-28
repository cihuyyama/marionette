# grok_farm

Python package for **Grok CLI manual thin/mass relogin** inside Marionette.

**Path:** `scripts/automation/grok_farm`  
**Not** part of the Rust proxy binary. Browser automation stays here only.

## Scope (v1)

| Does | Does not |
|------|----------|
| Email+password login on accounts.x.ai | Signup / Turnstile mass create |
| OAuth PKCE → access + refresh tokens | Write to 9Router SQLite |
| `verify_chat` ACTIVE probe | Playwright in Rust |
| 9Router-shaped JSON for `marionette-import` | Proactive scheduler (manual CLI only) |

## Pipeline

```
email|password
  -> Camoufox (humanize 0.8 headed / 1.0 headless)
  -> accounts.x.ai email login (Turnstile mouse path + hard Login click)
  -> auth.x.ai OAuth PKCE (redirect 127.0.0.1:56121 captured via route)
  -> exchange code → accessToken + refreshToken
  -> POST cli-chat-proxy.grok.com  "Reply with exactly ACTIVE"
  -> results/grok-accounts.json  (providerConnections, provider=grok-cli)
  -> just import-json <file>
```

## Setup

```powershell
cd scripts/automation
python -m venv grok_farm\.venv
.\grok_farm\.venv\Scripts\Activate.ps1
pip install -r grok_farm\requirements.txt
python -m camoufox fetch
copy grok_farm\.env.example grok_farm\.env
copy grok_farm\accounts.txt.example grok_farm\accounts.txt
```

## Run

`PYTHONPATH` = parent of the package (`scripts/automation`):

```powershell
cd scripts\automation
$env:PYTHONPATH = (Get-Location).Path
python -m grok_farm -f grok_farm\accounts.txt --json-progress --concurrency 2 --no-headless
```

### Flags

| Flag | Meaning |
|------|---------|
| `-f accounts.txt` | email\|password or JSONL lines |
| `-o out.json` | 9Router-shaped output |
| `--concurrency N` | parallel browsers |
| `--account-retries N` | full-pipeline retries (default 2) |
| `--account-delay S` | delay/stagger between accounts |
| `--skip-existing` | skip emails already in `-o` |
| `--skip-emails-file` | extra skip list |
| `--skip-verify` | skip chat ACTIVE probe (debug) |
| `--headless` / `--no-headless` | browser mode |
| `--json-progress` | NDJSON events for dashboard |
| `--proxy-file` | rotate proxies |
| `--debug` | verbose + screenshots on error |

## Import into Marionette

```bash
just import-json scripts/automation/grok_farm/results/grok-accounts.json
# or
cargo run --bin marionette-import -- --file scripts/automation/grok_farm/results/grok-accounts.json
```

## Export JSON shape

```json
{
  "providerConnections": [
    {
      "id": "…",
      "provider": "grok-cli",
      "email": "user@example.com",
      "name": "user@example.com",
      "isActive": true,
      "priority": 0,
      "accessToken": "…",
      "refreshToken": "…",
      "idToken": "…",
      "clientId": "b1a00492-073a-47ea-816f-4c329264a828",
      "expiresAt": "2026-…Z",
      "expiresIn": 21600,
      "scope": "openid profile …",
      "createdAt": "…",
      "updatedAt": "…"
    }
  ],
  "exportedAt": "…",
  "source": "marionette/scripts/automation/grok_farm",
  "count": 1
}
```

Matches `src/import_util.rs` `build_grok_data` field names.

## OTP / IMAP

If xAI shows an OTP form:

- Headed + no IMAP: waits ~60s for manual entry
- IMAP configured (`GROK_IMAP_*`): thin inbox scan for a code
- Headless + no IMAP: clear error (no silent hang)

## Turnstile (login)

Active path (ported from monolit, Camoufox only for now):

- Detect widget / “Verify you are human”
- Humanized `mouse.move` approach + jiggle + click (host text / iframe / CF frame)
- Soft remount on “Verification failed” (`turnstile.reset`)
- Re-fill password after CF remounts the form
- Hard pointer click on **Login** (not bare JS `el.click()`)

Still not ported: vision CAPTCHA for interactive puzzles; CloakBrowser engine (phase 2).

Ops tips when CF still fails:

```powershell
# concurrency 1, headed, residual proxy
$env:GROK_HUMANIZE="true"
$env:GROK_HUMANIZE_HEADED="1.0"
python -m grok_farm -f grok_farm\accounts.txt --concurrency 1 --no-headless --debug
```

## TODO stubs

- Optional CloakBrowser engine fallback
- Vision Turnstile solver for interactive puzzles
- Signup / mass create farm
- grok.com first-login “activate API” step (may help 403 accounts)
- Richer IMAP xAI subject parsers

## Notes

- Secrets: never commit `.env`, `accounts.txt`, `results/`, screenshots.
- Humanize default **on** (`GROK_HUMANIZE=true`); headed default **0.8**.
- Independent of `qoder_farm` (duplicated progress helpers on purpose).
