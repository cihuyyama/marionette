from __future__ import annotations

import asyncio
from typing import Any

from .config import Config
from .login import click_text_button, dismiss_cookie_banner
from .progress import Progress

_GROK_URL = "https://grok.com"


async def _grok_signed_in(page: Any) -> bool:
    # Authenticated grok.com renders the composer ("What do you want to know?")
    # and a Sign out affordance; a fresh/unprovisioned session shows Sign in /
    # Sign up instead. Distinguishing them is what makes activation truthful.
    try:
        if await page.locator("text=/Sign out/i").count() > 0:
            return True
        composer = await page.locator(
            "textarea, [contenteditable='true'], text=/What do you want to know/i"
        ).count()
        if composer == 0:
            composer = await page.locator(
                "[role='textbox'], text=/What's on your mind/i"
            ).count()
        if composer > 0:
            body = ((await page.inner_text("body")) or "").lower()
            if "sign in" not in body[:400] and "sign up" not in body[:400]:
                return True
    except Exception:
        pass
    return False


async def _sso_handoff_to_grok(page: Any) -> None:
    # A valid 'sso' cookie on accounts.x.ai does not by itself sign grok.com in;
    # grok.com needs its own session cookies set via the sign-in redirect, which
    # silently consumes the existing sso cookie. Walking these handoff URLs is
    # what flips a signed-out grok.com to authenticated (and provisions the
    # principal) without a fresh password login.
    for url in (
        "https://accounts.x.ai/sign-in?redirect=grok-com",
        "https://grok.com/",
    ):
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=30_000)
            await asyncio.sleep(2.0)
        except Exception:
            pass
    try:
        clicked = await click_text_button(
            page,
            ["Sign in", "Log in"],
            exclude=["Sign up", "Google", "Apple", "email"],
        )
        if clicked:
            await asyncio.sleep(2.5)
    except Exception:
        pass


async def activate_grok_if_needed(
    page: Any,
    cfg: Config,
    prog: Progress,
    label: str,
) -> bool:
    """Visit grok.com to create the Grok principal (entitlement).

    xAI OAuth consent requires a valid principalId. Accounts that have never
    visited grok.com have principalId="" and consent always returns
    "Failed to generate authentication code / Access denied". Loading the
    authenticated app + accepting terms creates the principal server-side.
    Returns True only when an authenticated grok.com session is confirmed.
    """
    prog.step(label, "activate", "ensure grok.com entitlement")
    for attempt in range(4):
        try:
            await page.goto(_GROK_URL, wait_until="domcontentloaded", timeout=30_000)
        except Exception:
            try:
                await page.goto(_GROK_URL, wait_until="commit", timeout=30_000)
            except Exception as exc:
                prog.log(f"activate skip (nav failed): {exc}", "WAIT", email=label)
                await asyncio.sleep(2.0)
                continue

        await asyncio.sleep(2.0)
        await dismiss_cookie_banner(page)

        if await _grok_signed_in(page):
            prog.log("activate: grok.com authenticated (principal ready)", "OK", email=label)
            await asyncio.sleep(1.0)
            return True

        body = ""
        try:
            body = ((await page.inner_text("body")) or "")[:2000].lower()
        except Exception:
            pass

        if "sign in" in body or "log in" in body or "sign up" in body:
            prog.log(
                f"activate: grok.com signed out — SSO handoff (attempt {attempt + 1}/4)",
                "WAIT",
                email=label,
            )
            await _sso_handoff_to_grok(page)
            if await _grok_signed_in(page):
                prog.log("activate: authenticated after SSO handoff", "OK", email=label)
                return True
        elif "i agree" in body or ("accept" in body and "cookie" not in body):
            clicked = await click_text_button(
                page,
                ["I agree", "Agree", "Accept", "Continue", "Get started", "Start"],
                exclude=["Deny", "Cancel", "Go back", "Sign out"],
            )
            if clicked:
                prog.log(f"activate: accepted terms ({clicked!r})", "OK", email=label)
                await asyncio.sleep(2.5)
                if await _grok_signed_in(page):
                    prog.log("activate: authenticated after terms", "OK", email=label)
                    return True
            else:
                prog.log("activate: terms page but no button found", "WAIT", email=label)

        await asyncio.sleep(2.5)

    try:
        cfg.screenshot_dir.mkdir(parents=True, exist_ok=True)
        safe = label.replace("@", "_at_").replace(".", "_")
        path = cfg.screenshot_dir / f"{safe}_activate_fail.png"
        await page.screenshot(path=str(path), full_page=True)
    except Exception:
        pass

    prog.log("activate: could not confirm authenticated grok.com session", "WARN", email=label)
    return False
