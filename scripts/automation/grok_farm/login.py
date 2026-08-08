from __future__ import annotations

import asyncio
import random
import re
from typing import Any

from .browser import recover_page_load_error
from .config import Config
from .progress import Progress


async def dismiss_cookie_banner(page: Any) -> None:
    """Dismiss accounts.x.ai cookie consent.

    accounts.x.ai stacks TWO layers: a visible custom banner (buttons
    labelled exactly "Reject All" / "Accept All" / "Cookie Settings", no
    ids) on top of the hidden OneTrust modal (#onetrust-* ids). The old
    OneTrust-id-first + "Accept All Cookies" fallback never matched the
    visible layer, leaving the banner blocking the signup buttons.

    Strategy: click any VISIBLE instance of the consent labels, several
    passes, until no cookie text remains. OneTrust-only pages still work
    via the id-based selectors.
    """
    async def _click_visible_keyword(kw: str) -> bool:
        try:
            loc = page.get_by_role("button", name=re.compile(rf"^{re.escape(kw)}$", re.I))
            n = await loc.count()
            for i in range(n):
                el = loc.nth(i)
                try:
                    if await el.is_visible():
                        await _hard_click_locator(page, el)
                        return True
                except Exception:
                    continue
        except Exception:
            pass
        return False

    for _ in range(4):
        clicked = False
        # Visible xAI custom layer first (exact labels), then OneTrust labels.
        for kw in ("Reject All", "Accept All", "Accept All Cookies", "Reject All Cookies"):
            if await _click_visible_keyword(kw):
                clicked = True
                break
        if not clicked:
            # OneTrust-only variant: ids, visibility-checked per instance.
            for sel in (
                "#onetrust-reject-all-handler",
                "#onetrust-accept-btn-handler",
                "#accept-recommended-btn-handler",
            ):
                try:
                    loc = page.locator(sel)
                    n = await loc.count()
                    for i in range(n):
                        el = loc.nth(i)
                        if await el.is_visible():
                            await _hard_click_locator(page, el)
                            clicked = True
                            break
                except Exception:
                    continue
                if clicked:
                    break
        if not clicked:
            break
        await asyncio.sleep(0.8)
        try:
            body = await page.evaluate("() => document.body.innerText")
            if "essential cookies" not in (body or "").lower():
                break
        except Exception:
            break


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


async def _restore_password_stage(
    page: Any,
    email_addr: str,
    password: str,
) -> bool:
    # Cloudflare remounts the login form after Turnstile and the password
    # input can vanish (or be recreated empty), so plain re-fill fails until
    # the email->Next stage is driven again and the field stabilises.
    if await _ensure_password_filled(page, password):
        return True
    try:
        if await page.locator("text=/Log( ?in|in) with email|Sign in with email/i").count() > 0:
            await click_login_with_email(page)
            await asyncio.sleep(0.8)
        email_loc = page.locator('input[type="email"], input[name="email"]')
        if await email_loc.count() > 0:
            await fill_input(
                page,
                ['input[type="email"]', 'input[name="email"]', 'input[autocomplete="email"]'],
                email_addr,
            )
            await asyncio.sleep(0.3)
            if await page.locator('input[type="password"]').count() == 0:
                try:
                    next_loc = page.get_by_role("button", name=re.compile(r"^next$", re.I))
                    if not await _hard_click_locator(page, next_loc):
                        await click_text_button(
                            page, ["Next", "Continue"], exclude=["Google", "Apple"]
                        )
                except Exception:
                    await click_text_button(page, ["Next", "Continue"], exclude=["Google", "Apple"])
                for _ in range(16):
                    await recover_page_load_error(page)
                    if await page.locator('input[type="password"]').count() > 0:
                        break
                    await asyncio.sleep(0.5)
                await asyncio.sleep(0.4)
    except Exception:
        pass
    for _ in range(3):
        if await _ensure_password_filled(page, password):
            return True
        await asyncio.sleep(0.7)
    return False


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
        if await page.locator("text=/CLOUDFLARE/i").count() > 0:
            if await page.locator('input[type="password"]').count() > 0:
                return True
        if await page.locator("iframe[src*='turnstile'], iframe[src*='challenges.cloudflare']").count() > 0:
            return True
        if await page.locator("[data-sitekey], .cf-turnstile").count() > 0:
            return True
        slot = await _turnstile_widget_box(page)
        return slot is not None
    except Exception:
        return False


async def _turnstile_widget_box(page: Any) -> dict[str, float] | None:
    try:
        box = await page.evaluate(
            """() => {
                const pick = (el) => {
                    if (!el) return null;
                    const r = el.getBoundingClientRect();
                    if (r.width < 40 || r.height < 12 || r.bottom <= 0) return null;
                    return { x: r.x, y: r.y, width: r.width, height: r.height };
                };
                const nodes = [...document.querySelectorAll('div,label,span,iframe')];
                for (const el of nodes) {
                    const t = (el.innerText || el.textContent || '').replace(/\\s+/g, ' ').trim();
                    if (!/Verify you are human/i.test(t)) continue;
                    if (t.length > 120) continue;
                    let cur = el;
                    for (let i = 0; i < 5 && cur; i++) {
                        const r = cur.getBoundingClientRect();
                        if (r.width >= 180 && r.width <= 520 && r.height >= 28 && r.height <= 90) {
                            return pick(cur);
                        }
                        cur = cur.parentElement;
                    }
                    return pick(el);
                }
                const ifr = document.querySelector(
                    'iframe[src*="challenges.cloudflare"], iframe[src*="turnstile"]'
                );
                if (ifr) return pick(ifr);
                const mount = document.querySelector('[data-sitekey], .cf-turnstile, #cf-turnstile');
                if (mount) return pick(mount);
                const pw = document.querySelector('input[type="password"]');
                const login = [...document.querySelectorAll('button, [role="button"]')].find(b =>
                    /^(login|log in|sign in)$/i.test((b.innerText || b.textContent || '').trim())
                );
                if (pw && login) {
                    const pr = pw.getBoundingClientRect();
                    const lr = login.getBoundingClientRect();
                    if (lr.top > pr.bottom + 20) {
                        const midY = (pr.bottom + lr.top) / 2;
                        const midX = (pr.left + pr.right) / 2;
                        return { x: midX - 140, y: midY - 22, width: 280, height: 44 };
                    }
                }
                return null;
            }"""
        )
        if box and isinstance(box, dict) and "x" in box:
            return {
                "x": float(box["x"]),
                "y": float(box["y"]),
                "width": float(box.get("width") or 0),
                "height": float(box.get("height") or 0),
            }
    except Exception:
        pass
    return None


async def turnstile_checkbox_checked(page: Any) -> bool:
    if await turnstile_token_len(page) > 20:
        return True
    try:
        for f in page.frames:
            url = (f.url or "").lower()
            if "challenges.cloudflare.com" not in url and "turnstile" not in url:
                continue
            try:
                state = await f.evaluate(
                    """() => {
                        const cb = document.querySelector(
                            'input[type="checkbox"], [role="checkbox"], label.cb-lb input'
                        );
                        if (cb) {
                            if (cb.checked === true) return true;
                            const aria = (cb.getAttribute('aria-checked') || '').toLowerCase();
                            if (aria === 'true') return true;
                        }
                        if (document.querySelector(
                            '#success, .success, [data-state="success"], .cb-lb input:checked, input:checked'
                        )) return true;
                        const mark = document.querySelector('.mark, #cf-stage, #success-text');
                        if (mark) {
                            const st = (mark.getAttribute('data-state') || '').toLowerCase();
                            if (st === 'success' || st === 'verified') return true;
                        }
                        const body = (document.body && document.body.innerText) || '';
                        if (/success/i.test(body) && !/Verify you are human/i.test(body)) return true;
                        return false;
                    }"""
                )
                if state:
                    return True
            except Exception:
                continue
    except Exception:
        pass
    try:
        return bool(
            await page.evaluate(
                """() => {
                    const host = document.querySelector(
                        '[data-state="success"], .cf-turnstile[data-state="success"]'
                    );
                    return !!host;
                }"""
            )
        )
    except Exception:
        return False


async def turnstile_solved(page: Any) -> bool:
    return (await turnstile_token_len(page) > 20) or (await turnstile_checkbox_checked(page))


async def turnstile_needs_click(page: Any) -> bool:
    if await turnstile_solved(page):
        return False
    if await turnstile_visible(page):
        return True
    try:
        if await page.locator("text=Verify you are human").count() > 0:
            return True
    except Exception:
        pass
    if await _turnstile_widget_box(page) is not None:
        return True
    return False


async def on_email_login_form(page: Any) -> bool:
    try:
        if await page.locator('input[type="password"]').count() == 0:
            return False
        if await page.locator("text=/Log in with your email/i").count() > 0:
            return True
        if await page.locator('input[type="email"], input[name="email"]').count() > 0:
            return True
        return await turnstile_visible(page)
    except Exception:
        return False


async def try_click_turnstile(
    page: Any,
    attempt: int = 0,
    prog: Progress | None = None,
    label: str = "",
) -> bool:
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
                        if prog:
                            prog.log(
                                f"ts click host text {sel} @({x:.0f},{y:.0f}) w={box['width']:.0f}",
                                "WAIT",
                                email=label or None,
                                step="login",
                            )
                        await page.mouse.move(x - 40, y - 20, steps=8)
                        await asyncio.sleep(random.uniform(0.15, 0.4))
                        await page.mouse.move(x, y, steps=10)
                        await asyncio.sleep(random.uniform(0.2, 0.5))
                        await page.mouse.click(x, y)
                        return True
            except Exception:
                continue

        for sel in (
            'iframe[src*="challenges.cloudflare.com"]',
            'iframe[src*="turnstile"]',
            "[data-sitekey]",
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
                if prog:
                    prog.log(
                        f"ts click container {sel} @({x:.0f},{y:.0f})",
                        "WAIT",
                        email=label or None,
                        step="login",
                    )
                await page.mouse.move(x - 50, y - 25, steps=8)
                await asyncio.sleep(random.uniform(0.15, 0.4))
                await page.mouse.move(x, y, steps=12)
                await asyncio.sleep(random.uniform(0.25, 0.6))
                await page.mouse.click(x, y)
                return True
            except Exception:
                continue

        for f in page.frames:
            if "challenges.cloudflare.com" not in (f.url or "") and "turnstile" not in (f.url or ""):
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
                    if prog:
                        prog.log(
                            f"ts click frame {sel} @({tx:.0f},{ty:.0f})",
                            "WAIT",
                            email=label or None,
                            step="login",
                        )
                    await page.mouse.move(tx, ty, steps=12)
                    await asyncio.sleep(random.uniform(0.2, 0.5))
                    await page.mouse.click(tx, ty)
                    return True
                except Exception:
                    continue
    except Exception as e:
        if prog:
            prog.log(f"ts click error: {e}", "WAIT", email=label or None, step="login")
        return False
    if prog:
        prog.log("ts click: no target found", "WAIT", email=label or None, step="login")
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


async def _soft_turnstile_remount(page: Any, password: str | None = None) -> None:
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
                document.querySelectorAll(
                    '[name="cf-turnstile-response"], textarea[name="cf-turnstile-response"], input[name*="turnstile"]'
                ).forEach(el => { try { el.value = ''; } catch (e) {} });
            }"""
        )
    except Exception:
        pass
    if password:
        try:
            await _ensure_password_filled(page, password)
        except Exception:
            pass
    await asyncio.sleep(random.uniform(1.2, 2.0))


async def wait_turnstile_passive(page: Any, *, max_wait: float = 12.0) -> bool:
    return await wait_turnstile_active(
        page, max_wait=max_wait, prog=None, label="", require_solved=True
    )


async def wait_turnstile_active(
    page: Any,
    *,
    max_wait: float = 35.0,
    prog: Progress | None = None,
    label: str = "",
    require_solved: bool = True,
    password: str | None = None,
) -> bool:
    deadline = asyncio.get_event_loop().time() + max_wait
    clicks = 0
    max_clicks = 8
    remounts = 0
    saw_widget = False

    while asyncio.get_event_loop().time() < deadline:
        tok = await turnstile_token_len(page)
        if tok > 20:
            if prog:
                prog.log(f"turnstile token ok (len={tok})", "OK", email=label or None, step="login")
            return True
        if await turnstile_checkbox_checked(page):
            await asyncio.sleep(0.5)
            if await turnstile_solved(page):
                if prog:
                    prog.log("turnstile checkbox checked", "OK", email=label or None, step="login")
                return True

        needs = await turnstile_needs_click(page)
        if needs:
            saw_widget = True

        if not needs and not saw_widget and not require_solved:
            return True
        if not needs and saw_widget and await turnstile_solved(page):
            return True
        if not needs and saw_widget and not await turnstile_visible(page):
            await asyncio.sleep(0.8)
            if await turnstile_solved(page) or not await turnstile_needs_click(page):
                return True

        if await _turnstile_verification_failed(page) and remounts < 4:
            if prog:
                prog.log(
                    f"turnstile verification failed — remount {remounts + 1}",
                    "WAIT",
                    email=label or None,
                    step="login",
                )
            await _soft_turnstile_remount(page, password=password)
            remounts += 1
            clicks = 0
            continue

        if (needs or saw_widget) and clicks < max_clicks:
            if clicks == 0:
                await asyncio.sleep(1.0)
            if prog:
                prog.log(
                    f"click turnstile checkbox ({clicks + 1}/{max_clicks})",
                    "WAIT",
                    email=label or None,
                    step="login",
                )
            clicked = await try_click_turnstile(page, clicks, prog=prog, label=label)
            clicks += 1
            await asyncio.sleep(2.5 if clicked else 1.0)
            tok2 = await turnstile_token_len(page)
            if prog:
                prog.log(
                    f"after click: ok={clicked} token_len={tok2} checked={await turnstile_checkbox_checked(page)}",
                    "DBG",
                    email=label or None,
                    step="login",
                )
            if await turnstile_solved(page):
                if prog:
                    prog.log("turnstile solved after click", "OK", email=label or None, step="login")
                return True
            if clicks >= 3 and remounts < 3 and (deadline - asyncio.get_event_loop().time()) > 8:
                if prog:
                    prog.log("turnstile still unchecked — remount", "WAIT", email=label or None, step="login")
                await _soft_turnstile_remount(page, password=password)
                remounts += 1
                clicks = 0
            continue

        if clicks >= max_clicks and remounts < 3 and saw_widget:
            if prog:
                prog.log("turnstile click budget — remount", "WAIT", email=label or None, step="login")
            await _soft_turnstile_remount(page, password=password)
            remounts += 1
            clicks = 0
            continue

        await asyncio.sleep(0.6)

    if await turnstile_solved(page):
        return True
    if require_solved and (saw_widget or await turnstile_needs_click(page)):
        return False
    return not await turnstile_needs_click(page)


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
    Drive accounts.x.ai email login form (Next -> password -> Turnstile -> Login).

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

    if not await _restore_password_stage(page, email_addr, password):
        prog.log("could not fill password before turnstile", "WAIT", email=label, step="login")

    for round_i in range(5):
        await recover_page_load_error(page)

        # A successful submit can navigate away mid-restore (redirect lands
        # while the password value is re-read), leaving no form controls
        # behind; detect that here instead of looping on "password empty".
        cur = (page.url or "").lower()
        if (
            await page.locator('input[type="password"]').count() == 0
            and await page.locator('input[type="email"], input[name="email"]').count() == 0
            and "sign-in" not in cur
        ):
            return True

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

        needs_ts = await turnstile_needs_click(page) or await turnstile_visible(page)
        if needs_ts or not await turnstile_solved(page):
            if await on_email_login_form(page) and (
                needs_ts
                or await page.locator("text=Verify you are human").count() > 0
                or await _turnstile_widget_box(page) is not None
            ):
                prog.log(
                    f"solving turnstile (round {round_i + 1})",
                    "WAIT",
                    email=label,
                    step="login",
                )
                ok_ts = await wait_turnstile_active(
                    page,
                    max_wait=32.0,
                    prog=prog,
                    label=label,
                    require_solved=True,
                    password=password,
                )
                if not ok_ts or not await turnstile_solved(page):
                    prog.log(
                        f"turnstile still unsolved (round {round_i + 1})",
                        "WAIT",
                        email=label,
                        step="login",
                    )
                    await try_click_turnstile(page, round_i, prog=prog, label=label)
                    await asyncio.sleep(1.5)
                    continue
                await asyncio.sleep(0.4)

        if not await _restore_password_stage(page, email_addr, password):
            prog.log(
                f"password empty after turnstile (round {round_i + 1})",
                "WAIT",
                email=label,
                step="login",
            )
            await asyncio.sleep(0.5)
            continue

        pw_now = await _password_field_value(page)
        if not pw_now:
            continue

        if await turnstile_needs_click(page) or (
            await on_email_login_form(page)
            and not await turnstile_solved(page)
            and (
                await turnstile_visible(page)
                or await page.locator("text=Verify you are human").count() > 0
                or await _turnstile_widget_box(page) is not None
            )
        ):
            prog.log(
                f"login blocked: checkbox still empty (round {round_i + 1})",
                "WAIT",
                email=label,
                step="login",
            )
            await try_click_turnstile(page, round_i, prog=prog, label=label)
            await asyncio.sleep(1.8)
            continue

        if await on_email_login_form(page) and not await turnstile_solved(page):
            prog.log(
                f"login blocked: no turnstile token yet (round {round_i + 1})",
                "WAIT",
                email=label,
                step="login",
            )
            await try_click_turnstile(page, round_i + 3, prog=prog, label=label)
            await wait_turnstile_active(
                page,
                max_wait=12.0,
                prog=prog,
                label=label,
                require_solved=True,
                password=password,
            )
            if not await turnstile_solved(page):
                continue

        prog.log(f"login submit round {round_i + 1}", "DBG", email=label, step="login")
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
            if await turnstile_needs_click(page) and await on_email_login_form(page):
                prog.log("still on form with empty checkbox after Login", "WAIT", email=label, step="login")
                continue
            if await page.locator("text=Log in with your email").count() == 0:
                if await turnstile_solved(page) or (
                    await page.locator("text=Verify you are human").count() == 0
                ):
                    if await page.locator('input[type="password"]').count() == 0:
                        return True
            cur = (page.url or "").lower()
            if "sign-in" not in cur and "login" not in cur:
                if "accounts.x.ai/sign" not in cur:
                    return True
            if await page.locator("text=/incorrect|invalid password|wrong password/i").count() > 0:
                prog.log("login rejected (wrong password?)", "ERR", email=label, step="login")
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
