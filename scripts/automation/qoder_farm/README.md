# qoder_farm

Python farm for **Qoder** accounts inside Marionette.

**Path:** `scripts/automation/qoder_farm`  
**Not** part of the Rust proxy binary. Browser automation stays here only.

## Pipeline

```
GSuite email|password
  -> Camoufox -> qoder.com Google SSO (password only, no OTP/IMAP)
  -> session gate (profile / not sign-in)
  -> Integrations -> New Token -> PAT (pt-...)
  -> optional device Continue (after PAT)
  -> openapi PAT exchange (jobToken / securityOauthToken)
  -> settle N seconds
  -> dudul.dev/inject (#key + #pat; Turnstile only if widget detected; retry <=5)
  -> results/qoder-accounts.json  (9Router providerConnections shape)
  -> NDJSON account_ok → Marionette imports that row into pool (if auto-import)
  -> job end: final re-import of full output (idempotent upsert)
```


## Dashboard

Marionette admin UI: **Qoder farm** nav.

- `POST /admin/farm/start` spawns `python -m qoder_farm ... --json-progress`
- Live progress polled from job events
- Optional auto-import into pool accounts

Env for server:

```
MARIONETTE_FARM_PYTHON=python
MARIONETTE_FARM_DIR=./scripts/automation/qoder_farm
MARIONETTE_FARM_DATA=./data/farm
```

## Setup (CLI)

```powershell
cd scripts/automation
# package is qoder_farm/ under this folder
python -m venv qoder_farm\.venv
.\qoder_farm\.venv\Scripts\Activate.ps1
pip install -r qoder_farm\requirements.txt
python -m camoufox fetch
copy qoder_farm\.env.example qoder_farm\.env
copy qoder_farm\accounts.txt.example qoder_farm\accounts.txt
```

Run (PYTHONPATH = parent of package):

```powershell
$env:PYTHONPATH = (Resolve-Path .).Path
python -m qoder_farm -f qoder_farm\accounts.txt --json-progress --no-headless
```

Or from package dir with parent on path:

```powershell
cd scripts\automation
$env:PYTHONPATH = (Get-Location).Path
python -m qoder_farm -f qoder_farm\accounts.txt --no-inject
```

## Flags

| Flag | Meaning |
|------|---------|
| `-f accounts.txt` | email\|password lines |
| `-o out.json` | 9Router-shaped output |
| `--inject` / `--no-inject` | dudul inject (`QODER_DUDUL_ACCESS_KEY` required) |
| `--inject-only --pat pt-…` | dudul inject for existing PAT (Accounts UI) |
| `--json-progress` | NDJSON events for dashboard |
| `--headless` / `--no-headless` | browser mode |
| `--device-auth` | device Continue (runs **after** PAT) |
| `--settle N` | seconds before inject |
| `--concurrency 1` | keep 1 for Turnstile |
| `--account-retries N` | full-pipeline retries per account (default 2) |
| `--account-delay S` | delay/stagger between accounts |
| `--skip-existing` | skip emails already in `-o` output |
| `--skip-emails-file` | extra skip list |
| `--proxy-file` | rotate proxies from file |

## Import

```bash
just import-json scripts/automation/qoder_farm/results/qoder-accounts.json
```

Or use dashboard **Import results** / auto-import.

## Notes

- Secrets: never commit `.env`, `accounts.txt`, `results/`, screenshots.
- Inject selectors are best-effort; PAT still saved if inject UI drifts.
- GSuite path has no IMAP/OTP.
