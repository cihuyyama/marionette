from __future__ import annotations

import asyncio
from typing import Any

from .config import Config
from .login import click_text_button, dismiss_cookie_banner
from .progress import Progress

_GROK_URL = "https://grok.com"


async def activate_grok_if_needed(
    page: Any,
    cfg: Config,
    prog: Progress,
    label: str,
) -> None:
    """Visit grok.com to create the Grok principal (entitlement).

    xAI OAuth consent requires a valid principalId. Accounts that have never
    visited grok.com have principalId="" and consent always returns
    "Failed to generate authentication code / Access denied".
    One visit + accepting terms creates the principal server-side.
    """
    prog.step(label, "activate", "ensure grok.com entitlement")
    try:
        await page.goto(_GROK_URL, wait_until="domcontentloaded", timeout=30_000)
    except Exception:
        try:
            await page.goto(_GROK_URL, wait_until="commit", timeout=30_000)
        except Exception as exc:
            prog.log(f"activate skip (nav failed): {exc}", "WAIT", email=label)
            return

    await asyncio.sleep(2.0)
    await dismiss_cookie_banner(page)

    body = ""
    try:
        body = (await page.inner_text("body"))[:2000].lower()
    except Exception:
        pass

    if "agree" in body or "terms" in body or "accept" in body or "continue" in body:
        clicked = await click_text_button(
            page,
            ["I agree", "Agree", "Accept", "Continue", "Get started", "Start"],
            exclude=["Deny", "Cancel", "Go back", "Sign out"],
        )
        if clicked:
            prog.log(f"activate: accepted terms ({clicked!r})", "OK", email=label)
            await asyncio.sleep(2.0)
        else:
            prog.log("activate: terms page but no button found", "WAIT", email=label)
    elif "sign in" in body or "log in" in body:
        prog.log("activate: not logged in at grok.com (will rely on OAuth login)", "WAIT", email=label)
    else:
        prog.log("activate: grok.com loaded (entitlement likely exists)", "OK", email=label)

    await asyncio.sleep(0.5)
