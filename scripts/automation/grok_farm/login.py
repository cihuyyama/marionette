from __future__ import annotations

import asyncio
import re
from typing import Any

from .browser import recover_page_load_error
from .config import Config
from .progress import Progress


async def dismiss_cookie_banner(page: Any) -> None:
    """OneTrust cookie modal blocks clicks — accept/reject early."""
    for sel in (
        "#onetrust-accept-btn-handler",
        "#onetrust-reject-all-handler",
        "#accept-recommended-btn-handler",
    ):
        try:
            btn = page.locator(sel).first
            if await btn.count() > 0 and await btn.is_visible():
                await btn.click(timeout=2000)
                await asyncio.sleep(0.4)
                return
        except Exception:
            continue
    try:
        await click_text_button(page, ["Accept All Cookies", "Reject All", "Allow All"])
    except Exception:
        pass


async def click_text_button(
    page: Any,
    keywords: list[str],
    exclude: list[str] | None = None,
) -> str | None:
    exclude = exclude or []
    for kw in keywords:
        try:
            loc = page.get_by_role("button", name=re.compile(rf"^{re.escape(kw)}$", re.I))
            if await loc.count() > 0 and await loc.first.is_visible():
                txt = (await loc.first.inner_text()).strip()
                if exclude and any(e.lower() in txt.lower() for e in exclude):
                    continue
                await loc.first.click()
                return txt
        except Exception:
            pass
        try:
            loc = page.get_by_role("button", name=re.compile(kw, re.I))
            if await loc.count() > 0 and await loc.first.is_visible():
                txt = (await loc.first.inner_text()).strip()
                if exclude and any(e.lower() in txt.lower() for e in exclude):
                    continue
                await loc.first.click()
                return txt
        except Exception:
            pass

    exclude_re = re.compile("|".join(re.escape(e) for e in exclude), re.I) if exclude else None
    try:
        return await page.evaluate(
            """({keywords, exclude}) => {
                const den = exclude ? new RegExp(exclude, 'i') : null;
                const btns = [...document.querySelectorAll(
                    'button, a, [role="button"], input[type="submit"]'
                )];
                for (const preferExact of [true, false]) {
                  for (const b of btns) {
                    const txt = (b.innerText || b.textContent || b.value || '').trim();
                    if (!txt) continue;
                    const rect = b.getBoundingClientRect();
                    if (rect.width <= 0 || rect.height <= 0) continue;
                    if (den && den.test(txt)) continue;
                    if (b.id && b.id.includes('onetrust')) continue;
                    if ((b.className || '').toString().includes('onetrust')) continue;
                    const low = txt.toLowerCase();
                    for (const kw of keywords) {
                        const k = kw.toLowerCase();
                        const hit = preferExact ? (low === k) : (low === k || low.includes(k));
                        if (hit) {
                            b.click();
                            return txt;
                        }
                    }
                  }
                }
                return null;
            }""",
            {"keywords": keywords, "exclude": exclude_re.pattern if exclude_re else ""},
        )
    except Exception:
        return None


async def fill_input(page: Any, selectors: list[str], value: str) -> bool:
    for sel in selectors:
        try:
            el = page.locator(sel).first
            if await el.count() == 0:
                continue
            if not await el.is_visible():
                continue
            await el.click()
            await el.fill("")
            await el.fill(value)
            await el.evaluate(
                """(el, v) => {
                    const setter = Object.getOwnPropertyDescriptor(
                        window.HTMLInputElement.prototype, 'value'
                    ).set;
                    setter.call(el, v);
                    el.dispatchEvent(new Event('input', { bubbles: true }));
                    el.dispatchEvent(new Event('change', { bubbles: true }));
                }""",
                value,
            )
            return True
        except Exception:
            continue
    try:
        ok = await page.evaluate(
            """({selectors, value}) => {
                for (const sel of selectors) {
                    const el = document.querySelector(sel);
                    if (!el) continue;
                    const rect = el.getBoundingClientRect();
                    if (rect.width <= 0 || rect.height <= 0) continue;
                    el.focus();
                    const setter = Object.getOwnPropertyDescriptor(
                        window.HTMLInputElement.prototype, 'value'
                    ).set;
                    setter.call(el, value);
                    el.dispatchEvent(new Event('input', { bubbles: true }));
                    el.dispatchEvent(new Event('change', { bubbles: true }));
                    return true;
                }
                return false;
            }""",
            {"selectors": selectors, "value": value},
        )
        return bool(ok)
    except Exception:
        return False


async def _password_field_value(page: Any) -> str:
    try:
        return str(
            await page.evaluate(
                """() => {
                    const el = document.querySelector('input[type="password"]');
                    return el ? (el.value || '') : '';
                }"""
            )
            or ""
        )
    except Exception:
        return ""


async def _ensure_password_filled(page: Any, password: str) -> bool:
    if not await fill_input(
        page,
        [
            'input[type="password"]',
            'input[name="password"]',
            'input[autocomplete="current-password"]',
        ],
        password,
    ):
        return False
    await asyncio.sleep(0.2)
    return bool(await _password_field_value(page))


async def turnstile_token_len(page: Any) -> int:
    try:
        return int(
            await page.evaluate(
                """() => {
                    const el = document.querySelector(
                        '[name="cf-turnstile-response"], textarea[name="cf-turnstile-response"]'
                    );
                    if (el && el.value) return el.value.length;
                    const inputs = document.querySelectorAll('input[type="hidden"]');
                    for (const i of inputs) {
                        if ((i.name || '').includes('turnstile') && i.value) return i.value.length;
                    }
                    return 0;
                }"""
            )
            or 0
        )
    except Exception:
        return 0


async def turnstile_visible(page: Any) -> bool:
    try:
        if await page.locator("text=Verify you are human").count() > 0:
            return True
        if await page.locator("iframe[src*='turnstile'], iframe[src*='challenges.cloudflare']").count() > 0:
            return True
        return await page.locator("[data-sitekey], .cf-turnstile").count() > 0
    except Exception:
        return False


async def wait_turnstile_passive(page: Any, *, max_wait: float = 12.0) -> bool:
    """
    Thin Turnstile wait — Camoufox humanize often solves it passively.
    Full vision/click solvers from the monolit kit are NOT ported (TODO if needed).
    """
    deadline = asyncio.get_event_loop().time() + max_wait
    while asyncio.get_event_loop().time() < deadline:
        if await turnstile_token_len(page) > 20:
            return True
        if not await turnstile_visible(page):
            # No widget — ok to proceed
            if await page.locator("text=Verify you are human").count() == 0:
                return True
        await asyncio.sleep(0.6)
    return await turnstile_token_len(page) > 20 or not await turnstile_visible(page)


async def click_login_with_email(page: Any) -> bool:
    """xAI UI uses both 'Sign in with email' and 'Login with email'."""
    clicked = await click_text_button(
        page,
        [
            "Login with email",
            "Log in with email",
            "Sign in with email",
            "Sign in with Email",
            "Continue with email",
        ],
        exclude=["Google", "Apple", "Microsoft", " with X", " with x"],
    )
    if clicked:
        return True
    try:
        await page.get_by_role(
            "button", name=re.compile(r"(log\s*in|sign\s*in)\s+with\s+email", re.I)
        ).click(timeout=4000)
        return True
    except Exception:
        return False


async def drive_email_password_login(
    page: Any,
    email_addr: str,
    password: str,
    prog: Progress,
    label: str,
) -> bool:
    """
    Drive accounts.x.ai email login form (Next → password → Turnstile wait → Login).

    Thin port of kit drive_email_password_login — no vision Turnstile solver.
    Always re-fill password after CF may remount the form.
    """
    await dismiss_cookie_banner(page)
    await recover_page_load_error(page)

    if await page.locator("text=/Log( ?in|in) with email|Sign in with email/i").count() > 0:
        if await page.locator('input[type="email"], input[type="password"]').count() == 0:
            await click_login_with_email(page)
            await asyncio.sleep(1.0)

    # Email step
    if await page.locator('input[type="email"], input[name="email"]').count() > 0:
        await fill_input(
            page,
            ['input[type="email"]', 'input[name="email"]', 'input[autocomplete="email"]'],
            email_addr,
        )
        await asyncio.sleep(0.3)
        if await page.locator('input[type="password"]').count() == 0:
            try:
                await page.get_by_role("button", name=re.compile(r"^next$", re.I)).click(timeout=4000)
            except Exception:
                await click_text_button(page, ["Next", "Continue"], exclude=["Google", "Apple"])
            for _ in range(20):
                await recover_page_load_error(page)
                if await page.locator('input[type="password"]').count() > 0:
                    break
                await asyncio.sleep(0.5)
            await asyncio.sleep(0.4)

    if not await _ensure_password_filled(page, password):
        prog.log("could not fill password before turnstile", "WAIT", email=label)

    for round_i in range(5):
        await recover_page_load_error(page)

        if (
            await page.locator('input[type="password"]').count() == 0
            and await page.locator(
                "text=/Login with email|Log in with email|Sign in with email/i"
            ).count()
            > 0
        ):
            await click_login_with_email(page)
            await asyncio.sleep(1.0)
            continue

        needs_ts = (
            await turnstile_visible(page)
            or await page.locator("text=Verify you are human").count() > 0
        )
        if needs_ts:
            prog.log(f"waiting turnstile (round {round_i + 1})", "WAIT", email=label, step="login")
            await wait_turnstile_passive(page, max_wait=18.0)

        if not await _ensure_password_filled(page, password):
            prog.log(f"password empty after turnstile (round {round_i + 1})", "WAIT", email=label)
            await asyncio.sleep(0.5)
            continue

        pw_now = await _password_field_value(page)
        tok_now = await turnstile_token_len(page)
        if not pw_now:
            continue
        if tok_now <= 20 and needs_ts:
            prog.log(f"login: waiting turnstile token (round {round_i + 1})", "WAIT", email=label)
            await asyncio.sleep(1.0)
            continue

        prog.log(f"login submit round {round_i + 1}", "DBG", email=label)
        try:
            await page.get_by_role(
                "button", name=re.compile(r"^(login|log in|sign in)$", re.I)
            ).click(timeout=4000)
        except Exception:
            await click_text_button(page, ["Login", "Log in", "Sign in", "Continue"])
        await asyncio.sleep(2.5)

        try:
            if await page.locator("text=Log in with your email").count() == 0:
                if (
                    await page.locator("text=Verify you are human").count() == 0
                    or await turnstile_token_len(page) > 20
                ):
                    if await page.locator('input[type="password"]').count() == 0:
                        return True
            cur = (page.url or "").lower()
            if "sign-in" not in cur and "login" not in cur:
                if "accounts.x.ai/sign" not in cur:
                    return True
            if await page.locator("text=/incorrect|invalid password|wrong password/i").count() > 0:
                prog.log("login rejected (wrong password?)", "ERR", email=label)
                await _ensure_password_filled(page, password)
        except Exception:
            return True
    return False


async def do_email_login(
    page: Any,
    email_addr: str,
    password: str,
    cfg: Config,
    prog: Progress,
    label: str,
) -> bool:
    """Login with email+password on accounts.x.ai if not already sessioned."""
    prog.step(label, "login", "email+password on accounts.x.ai")
    try:
        cur = page.url or ""
    except Exception:
        cur = ""

    # Session cookies?
    try:
        cookies = await page.context.cookies()
        has_sess = any(
            any(k in (c.get("name") or "").lower() for k in ("session", "auth", "token", "sid"))
            for c in cookies
        )
        if has_sess and "sign-in" not in cur and "sign-up" not in cur:
            prog.log("session cookies present — skip explicit login", "OK", email=label)
            return True
    except Exception:
        pass

    if "sign-in" not in cur and await page.locator('input[type="password"]').count() == 0:
        await page.goto(cfg.signin_url, wait_until="domcontentloaded", timeout=45_000)
        await asyncio.sleep(1.2)

    await dismiss_cookie_banner(page)
    await recover_page_load_error(page)
    await click_login_with_email(page)
    await asyncio.sleep(0.8)
    return await drive_email_password_login(page, email_addr, password, prog, label)
