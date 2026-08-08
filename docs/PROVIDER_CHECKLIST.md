# Provider port checklist

## Grok CLI

### Auth
- [ ] Confirm OAuth client id: `b1a00492-073a-47ea-816f-4c329264a828`
- [ ] Refresh: `POST https://auth.x.ai/oauth2/token` with refresh_token grant
- [ ] Parse `access_token`, `refresh_token`, `expires_in`, optional `id_token`
- [ ] Persist rotated refresh tokens if upstream returns new one
- [ ] Handle `invalid_grant` / `invalid_request` as dead account

### Chat upstream
- [ ] Copy base URL + path from 9Router grok-cli executor (verify live)
- [ ] Headers: Authorization Bearer access token; any client UA required
- [ ] Models: `grok-4.5`, `grok-4.5-high`, `grok-4.5-medium`, `grok-4.5-low` (confirm list)
- [ ] Non-stream JSON
- [ ] Stream adaptation to OpenAI chunks

### Quota / errors
- [ ] Detect free-usage 429 + markers (`actual/limit`, rolling reset text)
- [ ] Cooldown 25h on 429
- [ ] 402 credits / 403 access denied → disable/delete
- [ ] Do not spam disabled accounts

### Import
- [ ] From 9Router `providerConnections` where provider=`grok-cli`
- [ ] From farm `accounts.json` tokens (`access_token` snake_case)
- [ ] Dedupe by email

### References
- VPS: `/home/ubuntu/9router_wyx0/open-sse/...`
- Skill: `9router-instance-operations` / `references/grok-providers.md`
- Farm: `grok-farm/farm.py` `auto_inject_to_9router`

---

## Qoder

### Auth (critical)
- [ ] Read full `etteum-pool/src/proxy/providers/qoder.ts`
- [ ] Document required fields: personalToken, securityOauthToken/jobToken, userId, machineId
- [ ] Implement `ensureFreshAuth` equivalent
- [ ] Never assume raw `pt-` alone is enough for chat
- [ ] Keep machineId stable when refreshing

### Chat
- [ ] Model map: at least `lite` / `qd-Lite`
- [ ] Headers / body format from etteeum
- [ ] Stream + non-stream
- [ ] Error translation

### Import
- [ ] From etteeum DB/export if available
- [ ] Optional from 9Router qoder rows (map jt- correctly — see skill qoder-etteum-token-import)

### References
- `C:\Users\miqba\Downloads\gpt\etteum-pool\src\proxy\providers\qoder.ts`
- Skill: `etteum-pool-operations`
- Skill: `9router-instance-operations` → `references/qoder-etteum-token-import.md`

---

## Blackbox

### Auth
- [x] Static `sk-…` API key — `Authorization: Bearer`, no refresh/expiry (live-probed 2026-08)
- [x] Keys harvested at `app.blackbox.ai/keys` (signup → OTP → CREATE KEY)
- [x] `ensure_fresh_auth` = apiKey presence check only

### Chat upstream
- [x] `POST https://api.blackbox.ai/v1/chat/completions` — pure OpenAI shape
- [x] `GET /v1/models` public (no auth), ~123 models mixed chat/image/video
- [x] SSE standard (`data:` + `data: [DONE]`, usage in final chunk)
- [x] Models: `bb/<upstream-id>` public, upstream keeps own slashes (`bb/z-ai/glm-5.2` → `z-ai/glm-5.2`)
- [x] Routing branch **before** grok arm (upstream ids contain "grok")

### Quota / errors
- [x] quota kind `none` (no token budget; per-key RPM/TPM upstream-side)
- [x] Local `classify_blackbox_status`: 401→cut, 402→sealed+PaymentRequired, **403→fallen (moderation, never cut)**, 429→sealed w/ parsed `Try again in N seconds`
- [x] No same-account retry / no refresh worker

### Import / farm
- [x] `import_util` build_blackbox_data (apiKey required)
- [x] `blackbox_farm` = novabox flow (Playwright Chromium) + our CF temp-mail worker (OTP 6-digit, body-only scan)
- [x] Output 9Router-shaped `providerConnections` provider=blackbox; auto-import on `account_ok` NDJSON

### References
- `refs/novabox` (MIT) — signup/key-harvest flow source
- `docs.blackbox.ai/api-reference/*` — official error/rate-limit contract
- 9Router `open-sse/providers/registry/blackbox.js` — same lineage as our grok reference

---

## Shared OpenAI surface
- [ ] `messages[]` roles system/user/assistant/tool (v1 can start system+user only)
- [ ] `stream: true|false`
- [ ] `model` routing prefix
- [ ] usage fields best-effort
- [ ] cancellation: drop client → abort upstream request
