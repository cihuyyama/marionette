from __future__ import annotations

import asyncio
import re
from typing import Any
from urllib.parse import urlparse


def is_sign_in_url(url: str) -> bool:
    u = (url or "").lower()
    return any(
        part in u
        for part in (
            "/users/sign-in",
            "/users/sign_in",
            "/sign-in",
            "/signin",
            "/login",
            "accounts.google.com",
        )
    )


def is_qoder_account_host(url: str) -> bool:
    try:
        host = urlparse(url or "").netloc.lower()
    except Exception:
        return False
    return host.endswith("qoder.com") or host.endswith("qoder.sh")


async def recover_page_load_error(page: Any) -> bool:
    try:
        body = (await page.inner_text("body"))[:500].lower()
    except Exception:
        body = ""
    markers = (
        "couldn't load",
        "could not load",
        "page isn’t available",
        "page isn't available",
        "can't be reached",
        "cannot be reached",
        "took too long",
    )
    if not any(m in body for m in markers):
        if "reload" in body and ("try again" in body or "problem" in body):
            pass
        else:
            return False
    try:
        btn = page.get_by_role("button", name=re.compile(r"reload|try again", re.I))
        if await btn.count() > 0:
            await btn.first.click(timeout=3000)
        else:
            await page.reload(wait_until="domcontentloaded", timeout=45_000)
        await asyncio.sleep(1.5)
        return True
    except Exception:
        try:
            await page.reload(wait_until="domcontentloaded", timeout=45_000)
            await asyncio.sleep(1.5)
            return True
        except Exception:
            return False


async def current_url(page: Any) -> str:
    try:
        return page.url or ""
    except Exception:
        return ""


async def assert_not_sign_in(page: Any, *, context: str) -> None:
    await recover_page_load_error(page)
    url = await current_url(page)
    if is_sign_in_url(url):
        raise RuntimeError(f"session lost → sign-in ({context}) url={url[:120]}")


async def ensure_qoder_session(
    page: Any,
    *,
    profile_url: str = "https://qoder.com/account/profile",
    settle_s: float = 1.2,
) -> None:
    await recover_page_load_error(page)
    url = await current_url(page)
    if is_sign_in_url(url) or not is_qoder_account_host(url):
        try:
            await page.goto(profile_url, wait_until="domcontentloaded", timeout=30_000)
        except Exception as exc:
            raise RuntimeError(f"session gate: cannot open profile ({exc})") from exc
        await asyncio.sleep(settle_s)
        await recover_page_load_error(page)
        url = await current_url(page)

    if is_sign_in_url(url):
        raise RuntimeError(f"session gate failed → still on sign-in url={url[:120]}")

    if not is_qoder_account_host(url):
        try:
            await page.goto(profile_url, wait_until="domcontentloaded", timeout=30_000)
            await asyncio.sleep(settle_s)
        except Exception as exc:
            raise RuntimeError(f"session gate: host not qoder ({exc})") from exc
        url = await current_url(page)
        if is_sign_in_url(url) or not is_qoder_account_host(url):
            raise RuntimeError(f"session gate failed url={url[:120]}")

    try:
        body = (await page.inner_text("body"))[:400].lower()
    except Exception:
        body = ""
    if "sign in to qoder" in body or "continue with google" in body:
        if "integrations" not in url and "account" not in url:
            raise RuntimeError("session gate: sign-in chrome still visible")
