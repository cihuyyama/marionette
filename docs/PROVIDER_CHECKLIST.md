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

## Shared OpenAI surface
- [ ] `messages[]` roles system/user/assistant/tool (v1 can start system+user only)
- [ ] `stream: true|false`
- [ ] `model` routing prefix
- [ ] usage fields best-effort
- [ ] cancellation: drop client → abort upstream request
