from __future__ import annotations

import asyncio
import base64
import hashlib
import re
import secrets
import time
import uuid
from typing import Any

import aiohttp

from .config import Config
from .progress import Progress
from .session import assert_not_sign_in, current_url, ensure_qoder_session, recover_page_load_error


def _pkce() -> tuple[str, str]:
    verifier = secrets.token_urlsafe(32)
    digest = hashlib.sha256(verifier.encode()).digest()
    challenge = base64.urlsafe_b64encode(digest).rstrip(b"=").decode()
    return verifier, challenge


async def create_pat(page: Any, cfg: Config, prog: Progress, email: str) -> str:
    prog.step(email, "pat", "integrations")
    await ensure_qoder_session(page)
    await page.goto(cfg.integrations_url, wait_until="domcontentloaded", timeout=30_000)
    await asyncio.sleep(2.0)
    await recover_page_load_error(page)
    await assert_not_sign_in(page, context="integrations")

    url = await current_url(page)
    if "integrat" not in url.lower() and "account" not in url.lower():
        prog.log(f"integrations unexpected url={url[:100]}", "WAIT", email=email)

    new_token_btn = page.locator('button:has-text("New Token")').first
    try:
        if await new_token_btn.count() == 0:
            role_btn = page.get_by_role("button", name=re.compile(r"new\s*token", re.I))
            if await role_btn.count() > 0:
                new_token_btn = role_btn.first
    except Exception:
        pass

    try:
        await new_token_btn.wait_for(state="visible", timeout=25_000)
    except Exception as exc:
        url = await current_url(page)
        raise RuntimeError(
            f"New Token not found (session/UI) url={url[:120]}: {exc}"
        ) from exc

    await new_token_btn.click()
    await asyncio.sleep(1.0)

    name_input = page.locator('input[placeholder="Enter access token name"]').first
    await name_input.fill(f"marionette-{int(time.time())}")
    await asyncio.sleep(0.5)

    date_input = page.locator('input[placeholder="Set an expiration date"]').first
    await date_input.click()
    await asyncio.sleep(0.5)

    next_year_btn = page.locator('button[aria-label*="Next year"]').first
    try:
        if await next_year_btn.count() > 0:
            await next_year_btn.click()
            await asyncio.sleep(0.3)
    except Exception:
        pass

    day_cells = page.locator("td").filter(has_text=re.compile(r"^1$"))
    try:
        for i in range(await day_cells.count()):
            cell = day_cells.nth(i)
            ok = await cell.evaluate(
                "el => !el.classList.contains('disabled') && getComputedStyle(el).pointerEvents !== 'none'"
            )
            if ok:
                await cell.click()
                break
    except Exception:
        pass
    await asyncio.sleep(0.5)

    create_btn = page.locator('button:has-text("Create"):not([disabled])').first
    try:
        if await create_btn.count() == 0:
            create_btn = page.get_by_role("button", name=re.compile(r"^create$", re.I)).first
    except Exception:
        pass
    await create_btn.click()
    await asyncio.sleep(2.0)

    pat = await page.evaluate(
        """() => {
            const dialog = document.querySelector('[role="dialog"]');
            if (!dialog) return null;
            const text = dialog.textContent || '';
            const match = text.match(/pt-[A-Za-z0-9_-]+/);
            return match ? match[0] : null;
        }"""
    )
    if not pat:
        raise RuntimeError("failed to extract PAT from integrations dialog")
    if pat.endswith("I"):
        pat = pat[:-1]
    prog.log(f"PAT {pat[:12]}...", "OK", email=email)
    return pat


async def approve_device_auth(
    page: Any, cfg: Config, prog: Progress, email: str
) -> str:
    machine_id = str(uuid.uuid4())
    _verifier, challenge = _pkce()
    nonce = str(uuid.uuid4())
    device_url = (
        f"{cfg.device_auth_base}"
        f"?challenge={challenge}"
        f"&challenge_method=S256"
        f"&nonce={nonce}"
        f"&machine_id={machine_id}"
        f"&client_id={cfg.client_id}"
    )
    prog.step(email, "device_auth", "selectAccounts")
    await page.goto(device_url, wait_until="domcontentloaded", timeout=30_000)
    await asyncio.sleep(2.0)
    await recover_page_load_error(page)
    try:
        continue_btn = page.locator('button:has-text("Continue")').first
        if await continue_btn.count() == 0:
            continue_btn = page.get_by_role(
                "button", name=re.compile(r"^continue$", re.I)
            ).first
        if await continue_btn.count() > 0:
            await continue_btn.click()
            await asyncio.sleep(3.0)
            prog.log("device auth approved", "OK", email=email)
        else:
            prog.log("no Continue on device auth", "WAIT", email=email)
    except Exception as exc:
        prog.log(f"device auth err: {exc}", "DBG", email=email)
    return machine_id


async def exchange_pat(pat: str, cfg: Config) -> dict[str, Any]:
    async with aiohttp.ClientSession() as http:
        resp = await http.post(
            cfg.pat_exchange_url,
            json={"personal_token": pat},
            headers={
                "Content-Type": "application/json",
                "Accept": "application/json",
            },
        )
        body = await resp.text()
        if resp.status != 200:
            raise RuntimeError(f"PAT exchange failed: {resp.status} {body[:200]}")
        data = await resp.json()
    token = data.get("token") or data.get("access_token") or ""
    if not token:
        raise RuntimeError("PAT exchange returned no token")
    return {
        "securityOauthToken": token,
        "refreshToken": data.get("refresh_token") or data.get("refreshToken") or "",
        "raw": data,
    }


async def fetch_quota(oauth_token: str, cfg: Config) -> dict[str, Any] | None:
    if not oauth_token:
        return None
    headers = {
        "Accept": "application/json",
        "Authorization": f"Bearer {oauth_token}",
        "Cosy-ClientType": "5",
        "Cosy-Version": "1.0.8",
        "User-Agent": "qodercli/1.0.8",
    }
    async with aiohttp.ClientSession() as http:
        resp = await http.get(cfg.quota_url, headers=headers)
        if resp.status != 200:
            return None
        quota_data = await resp.json()
        resp2 = await http.get(cfg.plan_url, headers=headers)
        plan_data = await resp2.json() if resp2.status == 200 else {}
    user_quota = quota_data.get("userQuota") or {}
    return {
        "quotaLimit": user_quota.get("total", 0),
        "quotaRemaining": user_quota.get("remaining", 0),
        "plan": plan_data.get("plan_tier_name", "Community"),
        "isQuotaExceeded": quota_data.get("isQuotaExceeded", False),
    }
