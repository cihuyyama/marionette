from __future__ import annotations

import asyncio
import random
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
                await _hard_click_locator(page, btn)
                await asyncio.sleep(0.4)
                return
        except Exception:
            continue
    try:
        await click_text_button(page, ["Accept All Cookies", "Reject All", "Allow All"])
    except Exception:
        pass


async def _hard_click_locator(page: Any, loc: Any, *, double: bool = False) -> bool:
    try:
        target = loc.first if hasattr(loc, "first") else loc
        try:
            if await target.count() == 0:
                return False
        except Exception:
            pass
        try:
            await target.scroll_into_view_if_needed(timeout=2000)
        except Exception:
            pass
        box = None
        try:
            box = await target.bounding_box()
        except Exception:
            box = None
        try:
            await target.click(timeout=4000, delay=80)
            if double:
                await asyncio.sleep(0.15)
                await target.click(timeout=3000, delay=60)
            return True
        except Exception:
            pass
        if box:
            x = box["x"] + box["width"] / 2
            y = box["y"] + box["height"] / 2
            try:
                await page.mouse.move(x - random.uniform(12, 36), y - random.uniform(8, 22), steps=6)
                await asyncio.sleep(random.uniform(0.05, 0.12))
                await page.mouse.move(x, y, steps=8)
                await asyncio.sleep(random.uniform(0.06, 0.14))
                await page.mouse.down()
                await asyncio.sleep(random.uniform(0.04, 0.08))
                await page.mouse.up()
                if double:
                    await asyncio.sleep(0.12)
                    await page.mouse.down()
                    await asyncio.sleep(0.04)
                    await page.mouse.up()
                return True
            except Exception:
                pass
        try:
            await target.click(timeout=2000, force=True)
            return True
        except Exception:
            pass
        try:
            await target.evaluate(
                """(el) => {
                    el.focus();
                    for (const type of ['pointerdown','mousedown','pointerup','mouseup','click']) {
                        el.dispatchEvent(new MouseEvent(type, {
                            bubbles: true, cancelable: true, view: window
                        }));
                    }
                }"""
            )
            return True
        except Exception:
            return False
    except Exception:
        return False


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
                if await _hard_click_locator(page, loc):
                    return txt
        except Exception:
            pass
        try:
            loc = page.get_by_role("button", name=re.compile(kw, re.I))
            if await loc.count() > 0 and await loc.first.is_visible():
                txt = (await loc.first.inner_text()).strip()
                if exclude and any(e.lower() in txt.lower() for e in exclude):
                    continue
                if await _hard_click_locator(page, loc):
                    return txt
        except Exception:
            pass

    exclude_re = re.compile("|".join(re.escape(e) for e in exclude), re.I) if exclude else None
    try:
        handle = await page.evaluate(
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
                            return { txt, x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
                        }
                    }
                  }
                }
                return null;
            }""",
            {"keywords": keywords, "exclude": exclude_re.pattern if exclude_re else ""},
        )
        if handle and isinstance(handle, dict) and "x" in handle:
            x = float(handle["x"])
            y = float(handle["y"])
            await page.mouse.move(x - 20, y - 12, steps=6)
            await asyncio.sleep(random.uniform(0.05, 0.12))
            await page.mouse.move(x, y, steps=8)
            await asyncio.sleep(random.uniform(0.05, 0.12))
            await page.mouse.click(x, y)
            return str(handle.get("txt") or "")
        return None
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


async def try_click_turnstile(page: Any, attempt: int = 0) -> bool:
    try:
        for sel in (
            'text=Verify you are human',
            'label:has-text("Verify you are human")',
            '[aria-label*="Verify you are human" i]',
        ):
            try:
                loc = page.locator(sel).first
                if await loc.count() > 0 and await loc.is_visible():
                    box = await loc.bounding_box(timeout=2000)
                    if box:
                        x = box["x"] + min(18, box["width"] * 0.15)
                        y = box["y"] + box["height"] / 2
                        await page.mouse.move(x - 40, y - 20, steps=8)
                        await asyncio.sleep(random.uniform(0.15, 0.4))
                        jx = random.uniform(-3, 3)
                        jy = random.uniform(-2, 2)
                        await page.mouse.move(x + jx, y + jy, steps=10)
                        await asyncio.sleep(random.uniform(0.12, 0.35))
                        await page.mouse.move(x, y, steps=4)
                        await asyncio.sleep(random.uniform(0.15, 0.4))
                        await page.mouse.click(x, y, delay=random.randint(40, 90))
                        return True
            except Exception:
                continue

        for sel in (
            'iframe[src*="challenges.cloudflare.com"]',
            'iframe[src*="turnstile"]',
            "[data-sitekey]",
            ".cf-turnstile",
            'div:has(iframe[src*="challenges.cloudflare"])',
        ):
            try:
                loc = page.locator(sel).first
                if await loc.count() == 0:
                    continue
                box = await loc.bounding_box(timeout=2000)
                if not box:
                    continue
                x = box["x"] + min(28, max(12, box["width"] * 0.12))
                y = box["y"] + box["height"] / 2
                await page.mouse.move(x - 50, y - 25, steps=8)
                await asyncio.sleep(random.uniform(0.15, 0.4))
                await page.mouse.move(
                    x + random.uniform(-2, 2),
                    y + random.uniform(-2, 2),
                    steps=12,
                )
                await asyncio.sleep(random.uniform(0.2, 0.5))
                await page.mouse.move(x, y, steps=3)
                await asyncio.sleep(random.uniform(0.1, 0.25))
                await page.mouse.click(x, y, delay=random.randint(50, 100))
                return True
            except Exception:
                continue

        for f in page.frames:
            url = f.url or ""
            if "challenges.cloudflare.com" not in url and "turnstile" not in url:
                continue
            for sel in (
                'input[type="checkbox"]',
                "label.cb-lb input",
                'label input[type="checkbox"]',
                '[role="checkbox"]',
                "body",
            ):
                try:
                    loc = f.locator(sel).first
                    if await loc.count() == 0:
                        continue
                    box = await loc.bounding_box(timeout=2000)
                    if not box:
                        continue
                    tx = box["x"] + min(20, box["width"] * 0.2)
                    ty = box["y"] + box["height"] / 2
                    await page.mouse.move(tx - 30, ty - 15, steps=8)
                    await asyncio.sleep(random.uniform(0.1, 0.3))
                    await page.mouse.move(tx, ty, steps=12)
                    await asyncio.sleep(random.uniform(0.15, 0.4))
                    await page.mouse.click(tx, ty, delay=random.randint(40, 90))
                    return True
                except Exception:
                    continue
    except Exception:
        return False
    return False


async def _turnstile_verification_failed(page: Any) -> bool:
    try:
        if await page.locator("text=/Verification failed/i").count() > 0:
            return True
        if await page.locator("text=/Troubleshoot/i").count() > 0:
            body = (await page.inner_text("body"))[:2500]
            if re.search(r"Verification failed|CLOUDFLARE", body, re.I):
                return True
        return False
    except Exception:
        return False


async def _soft_turnstile_remount(page: Any) -> None:
    try:
        if await page.locator("text=/Verification failed/i").count() > 0:
            for sel in (
                "text=Troubleshoot",
                'a:has-text("Troubleshoot")',
                "text=/try again/i",
            ):
                try:
                    loc = page.locator(sel).first
                    if await loc.count() > 0 and await loc.is_visible():
                        await _hard_click_locator(page, loc)
                        await asyncio.sleep(1.2)
                        break
                except Exception:
                    continue
    except Exception:
        pass
    try:
        await page.evaluate(
            """() => {
                try {
                    if (window.turnstile && typeof window.turnstile.reset === 'function') {
                        window.turnstile.reset();
                    }
                } catch (e) {}
            }"""
        )
    except Exception:
        pass
    await asyncio.sleep(random.uniform(0.6, 1.2))


async def wait_turnstile_passive(page: Any, *, max_wait: float = 12.0) -> bool:
    return await wait_turnstile_active(page, max_wait=max_wait, prog=None, label="")


async def wait_turnstile_active(
    page: Any,
    *,
    max_wait: float = 22.0,
    prog: Progress | None = None,
    label: str = "",
) -> bool:
    deadline = asyncio.get_event_loop().time() + max_wait
    clicks = 0
    max_clicks = 6
    remounts = 0

    while asyncio.get_event_loop().time() < deadline:
        if await turnstile_token_len(page) > 20:
            return True
        visible = await turnstile_visible(page)
        if not visible and await page.locator("text=Verify you are human").count() == 0:
            return True

        if await _turnstile_verification_failed(page) and remounts < 3:
            if prog:
                prog.log(
                    f"turnstile verification failed — remount {remounts + 1}",
                    "WAIT",
                    email=label or None,
                    step="login",
                )
            await _soft_turnstile_remount(page)
            remounts += 1
            continue

        if visible and clicks < max_clicks:
            if prog and clicks == 0:
                prog.log("turnstile mouse click", "WAIT", email=label or None, step="login")
            if clicks == 0:
                await asyncio.sleep(random.uniform(0.8, 1.6))
            clicked = await try_click_turnstile(page, clicks)
            clicks += 1
            if clicked:
                await asyncio.sleep(random.uniform(1.0, 2.2))
            else:
                await asyncio.sleep(random.uniform(0.5, 1.0))
            if await turnstile_token_len(page) > 20:
                return True
            continue

        await asyncio.sleep(0.6)

    tok = await turnstile_token_len(page)
    if tok > 20:
        return True
    return not await turnstile_visible(page)


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
        loc = page.get_by_role(
            "button", name=re.compile(r"(log\s*in|sign\s*in)\s+with\s+email", re.I)
        )
        return await _hard_click_locator(page, loc)
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
    Drive accounts.x.ai email login form (Next → password → Turnstile → Login).

    Active Turnstile mouse path + hard Login click. Always re-fill password after CF remount.
    """
    await dismiss_cookie_banner(page)
    await recover_page_load_error(page)

    if await page.locator("text=/Log( ?in|in) with email|Sign in with email/i").count() > 0:
        if await page.locator('input[type="email"], input[type="password"]').count() == 0:
            await click_login_with_email(page)
            await asyncio.sleep(1.0)

    if await page.locator('input[type="email"], input[name="email"]').count() > 0:
        await fill_input(
            page,
            ['input[type="email"]', 'input[name="email"]', 'input[autocomplete="email"]'],
            email_addr,
        )
        await asyncio.sleep(0.3)
        if await page.locator('input[type="password"]').count() == 0:
            try:
                loc = page.get_by_role("button", name=re.compile(r"^next$", re.I))
                if not await _hard_click_locator(page, loc):
                    await click_text_button(page, ["Next", "Continue"], exclude=["Google", "Apple"])
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
            prog.log(
                f"solving turnstile (round {round_i + 1})",
                "WAIT",
                email=label,
                step="login",
            )
            ok_ts = await wait_turnstile_active(
                page, max_wait=22.0, prog=prog, label=label
            )
            if not ok_ts and await turnstile_token_len(page) <= 20:
                prog.log(
                    f"turnstile still unsolved (round {round_i + 1})",
                    "WAIT",
                    email=label,
                    step="login",
                )

        if not await _ensure_password_filled(page, password):
            prog.log(f"password empty after turnstile (round {round_i + 1})", "WAIT", email=label)
            await asyncio.sleep(0.5)
            continue

        pw_now = await _password_field_value(page)
        tok_now = await turnstile_token_len(page)
        still_needs = (
            await turnstile_visible(page)
            or await page.locator("text=Verify you are human").count() > 0
        )
        if not pw_now:
            continue
        if tok_now <= 20 and still_needs:
            prog.log(
                f"login: turnstile token missing — click again (round {round_i + 1})",
                "WAIT",
                email=label,
            )
            await try_click_turnstile(page, round_i)
            await asyncio.sleep(1.2)
            continue

        prog.log(f"login submit round {round_i + 1}", "DBG", email=label)
        submitted = False
        try:
            loc = page.get_by_role(
                "button", name=re.compile(r"^(login|log in|sign in)$", re.I)
            )
            submitted = await _hard_click_locator(page, loc)
        except Exception:
            submitted = False
        if not submitted:
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
