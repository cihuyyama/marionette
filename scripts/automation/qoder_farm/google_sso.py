from __future__ import annotations

import asyncio
import random
import re
import time
from typing import Any
from urllib.parse import urlparse

from .progress import Progress
from .session import recover_page_load_error


async def _wait_visible(page: Any, selector: str, timeout: float = 25_000) -> Any | None:
    try:
        loc = page.locator(selector).first
        await loc.wait_for(state="visible", timeout=timeout)
        return loc
    except Exception:
        return None


async def _straight_move_click(page: Any, x: float, y: float, *, double: bool = False) -> bool:
    try:
        await page.mouse.move(x, y, steps=2)
        await asyncio.sleep(0.05)
        await page.mouse.down()
        await asyncio.sleep(0.04)
        await page.mouse.up()
        if double:
            await asyncio.sleep(0.1)
            await page.mouse.down()
            await asyncio.sleep(0.04)
            await page.mouse.up()
        return True
    except Exception:
        return False


async def _hard_click_locator(page: Any, loc: Any, *, double: bool = False) -> bool:
    try:
        target = loc.first
        if await target.count() == 0:
            return False
        try:
            await target.wait_for(state="visible", timeout=8_000)
        except Exception:
            pass
        try:
            await target.scroll_into_view_if_needed(timeout=2000)
        except Exception:
            pass
        await asyncio.sleep(0.15)

        try:
            await target.click(timeout=5000, delay=40)
            if double:
                await asyncio.sleep(0.12)
                await target.click(timeout=3000, delay=40)
            return True
        except Exception:
            pass

        box = None
        try:
            box = await target.bounding_box()
        except Exception:
            box = None
        if box and box.get("width", 0) > 1 and box.get("height", 0) > 1:
            x = box["x"] + box["width"] * 0.5
            y = box["y"] + box["height"] * 0.5
            if await _straight_move_click(page, x, y, double=double):
                return True

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


async def _human_click(page: Any, locator: Any, *, steps: int | None = None) -> bool:
    _ = steps
    return await _hard_click_locator(page, locator)


async def _human_click_selector(page: Any, selector: str) -> bool:
    try:
        return await _hard_click_locator(page, page.locator(selector))
    except Exception:
        return False


async def _human_click_by_text(
    page: Any,
    keywords: list[str],
    *,
    require_body: list[str] | None = None,
) -> bool:
    try:
        if require_body:
            body = (
                await page.evaluate(
                    "() => (document.body && document.body.innerText || '').toLowerCase()"
                )
            ) or ""
            body = str(body).lower()
            if not any(k in body for k in require_body):
                return False
    except Exception:
        if require_body:
            return False

    for kw in keywords:
        try:
            loc = page.get_by_role(
                "button", name=re.compile(rf"^{re.escape(kw)}$", re.I)
            )
            if await loc.count() > 0 and await loc.first.is_visible():
                if await _hard_click_locator(page, loc):
                    return True
        except Exception:
            pass
        try:
            loc = page.get_by_role("button", name=re.compile(re.escape(kw), re.I))
            if await loc.count() > 0 and await loc.first.is_visible():
                if await _hard_click_locator(page, loc):
                    return True
        except Exception:
            pass
        for sel in (
            f'button:has-text("{kw}")',
            f'div[role="button"]:has-text("{kw}")',
            f'span[role="button"]:has-text("{kw}")',
            f'a:has-text("{kw}")',
        ):
            try:
                loc = page.locator(sel)
                if await loc.count() == 0:
                    continue
                if await loc.first.is_visible() and await _hard_click_locator(page, loc):
                    return True
            except Exception:
                continue

    try:
        hit = await page.evaluate(
            """(keywords) => {
                const btns = [...document.querySelectorAll(
                    'button, a, [role="button"], input[type="submit"]'
                )];
                for (const preferExact of [true, false]) {
                  for (const b of btns) {
                    const txt = (b.innerText || b.textContent || b.value || '').trim();
                    if (!txt) continue;
                    const rect = b.getBoundingClientRect();
                    if (rect.width <= 0 || rect.height <= 0) continue;
                    const low = txt.toLowerCase();
                    for (const kw of keywords) {
                      const k = String(kw).toLowerCase();
                      const hit = preferExact ? (low === k) : (low === k || low.includes(k));
                      if (hit) {
                        return {
                          x: rect.x + rect.width / 2,
                          y: rect.y + rect.height / 2,
                          txt,
                        };
                      }
                    }
                  }
                }
                return null;
            }""",
            keywords,
        )
        if hit and isinstance(hit, dict) and "x" in hit and "y" in hit:
            return await _straight_move_click(page, float(hit["x"]), float(hit["y"]))
    except Exception:
        pass
    return False


async def _is_password_step(target: Any) -> bool:
    try:
        return bool(
            await target.evaluate(
                """() => {
                    for (const el of document.querySelectorAll('input[type="password"], input[name="Passwd"]')) {
                        if (el.offsetParent !== null) return true;
                    }
                    return false;
                }"""
            )
        )
    except Exception:
        return False


async def _is_email_step(target: Any) -> bool:
    try:
        return bool(
            await target.evaluate(
                """() => {
                    for (const el of document.querySelectorAll('input[type="email"], input[name="identifier"], #identifierId')) {
                        if (el.offsetParent !== null) return true;
                    }
                    return false;
                }"""
            )
        )
    except Exception:
        return False


async def _click_google_next(page: Any) -> bool:
    for selector in (
        "#identifierNext button",
        "#passwordNext button",
        "#identifierNext",
        "#passwordNext",
    ):
        if await _human_click_selector(page, selector):
            return True
    return await _human_click_by_text(page, ["Next", "Berikutnya", "Lanjut"])


async def _fill_google_email_step(page: Any, email: str) -> bool:
    try:
        try:
            await page.wait_for_load_state("domcontentloaded", timeout=15_000)
        except Exception:
            pass
        locator = await _wait_visible(
            page,
            '#identifierId, input[type="email"], input[name="identifier"]',
            timeout=20_000,
        )
        if locator is None:
            return False
        await asyncio.sleep(0.4)
        await locator.scroll_into_view_if_needed()
        try:
            await locator.click(timeout=4000, delay=30)
        except Exception:
            if not await _human_click(page, locator):
                return False
        await asyncio.sleep(0.2)
        try:
            await locator.fill("")
        except Exception:
            try:
                await locator.press("Control+a")
                await locator.press("Backspace")
            except Exception:
                pass
        await locator.press_sequentially(email, delay=random.randint(25, 45))
        await asyncio.sleep(0.35)
        value = await locator.input_value()
        if email.lower() != str(value).lower().strip():
            try:
                await locator.fill(email)
            except Exception:
                return False
            value = await locator.input_value()
            if email.lower() != str(value).lower().strip():
                return False
        await asyncio.sleep(0.25)
        if not await _click_google_next(page):
            await locator.press("Enter")
        try:
            await page.wait_for_load_state("domcontentloaded", timeout=12_000)
        except Exception:
            pass
        await asyncio.sleep(0.8)
        return True
    except Exception:
        return False


async def _fill_google_password_step(page: Any, password: str) -> bool:
    try:
        await page.wait_for_load_state("domcontentloaded", timeout=15_000)
    except Exception:
        pass
    for selector in ['input[name="Passwd"]', 'input[type="password"]']:
        try:
            locator = await _wait_visible(page, selector, timeout=18_000)
            if locator is None:
                continue
            await asyncio.sleep(0.45)
            await locator.scroll_into_view_if_needed()
            try:
                await locator.click(timeout=4000, delay=30)
            except Exception:
                if not await _human_click(page, locator):
                    continue
            await asyncio.sleep(0.2)
            try:
                await locator.fill("")
            except Exception:
                try:
                    await locator.press("Control+a")
                    await locator.press("Backspace")
                except Exception:
                    pass
            await locator.press_sequentially(password, delay=random.randint(25, 45))
            await asyncio.sleep(0.35)
            await asyncio.sleep(0.2)
            if not await _click_google_next(page):
                await locator.press("Enter")
            try:
                await page.wait_for_load_state("domcontentloaded", timeout=12_000)
            except Exception:
                pass
            await asyncio.sleep(0.9)
            return True
        except Exception:
            continue
    return False


async def _handle_google_gaplustos(page: Any) -> bool:
    try:
        current_url = page.url
    except Exception:
        current_url = ""
    if "/speedbump/gaplustos" not in current_url:
        return False
    for selector in [
        "#gaplustosNext button",
        "#confirm",
        'input[name="confirm"]',
        'input[type="submit"]',
    ]:
        if await _human_click_selector(page, selector):
            return True
    return await _human_click_by_text(page, ["I understand", "I agree", "Continue", "Next"])


async def _handle_workspace_welcome(page: Any) -> bool:
    try:
        current_url = page.url
    except Exception:
        current_url = ""
    if "accounts.google.com" not in current_url and "google.com" not in current_url:
        return False
    ok = await _human_click_by_text(
        page,
        [
            "I understand",
            "I Understand",
            "Saya mengerti",
            "Mengerti",
            "I agree",
            "Continue",
            "Accept",
        ],
        require_body=[
            "welcome to your new account",
            "your organization administrator manages this account",
            "administrator decides which google workspace",
        ],
    )
    if ok:
        return True
    for sel in ("#confirm", "#confirmNext button", 'input[name="confirm"]'):
        if await _human_click_selector(page, sel):
            return True
    return False


def _google_url_looks_like_consent(url: str) -> bool:
    u = (url or "").lower()
    if "accounts.google.com" not in u and "google.com" not in u:
        return False
    markers = (
        "/signin/oauth",
        "/o/oauth2",
        "oauth/consent",
        "consent",
        "approval",
        "brandaccount",
        "interactiveconsent",
        "programmatic_auth",
        "servicelogin",
        "speedbump",
        "wap/consent",
    )
    return any(m in u for m in markers)


async def _page_text_lower(page: Any) -> str:
    try:
        body = await page.evaluate(
            "() => (document.body && document.body.innerText || '').toLowerCase()"
        )
        return str(body or "").lower()
    except Exception:
        return ""


_CONSENT_PRIMARY_LABELS = (
    "continue",
    "allow",
    "i understand",
    "i agree",
    "accept",
    "confirm",
    "lanjut",
    "izinkan",
    "setuju",
    "mengerti",
)
_CONSENT_NEGATIVE_LABELS = (
    "cancel",
    "deny",
    "reject",
    "not now",
    "no thanks",
    "back",
    "batal",
    "tolak",
    "nanti",
    "learn more",
    "pelajari",
)


async def _looks_like_google_consent_ui(page: Any) -> bool:
    try:
        url = page.url
    except Exception:
        url = ""
    if "accounts.google.com" not in (url or "") and "google.com" not in (url or ""):
        return False
    if await _is_password_step(page) or await _is_email_step(page):
        return False
    if _google_url_looks_like_consent(url):
        return True
    body = await _page_text_lower(page)
    if not body:
        return False
    phrases = (
        "sign in to qoder",
        "sign in to ",
        "wants to access your google account",
        "want to access your google account",
        "google will allow",
        "google will allow qoder",
        "make sure you trust",
        "review qoder",
        "review this app",
        "this app wants",
        "see your email address",
        "see your personal info",
        "name and profile picture",
        "associate you with your personal info",
        "allow qoder",
        "continue to qoder",
        "learn more about sign in with google",
        "izin akses",
        "ingin mengakses",
        "akses ke akun google",
        "pastikan anda memercayai",
        "select all",
        "pilih semua",
    )
    if any(p in body for p in phrases):
        return True
    try:
        if await page.locator("#submit_approve_access, #submit_approve_access button").count() > 0:
            return True
    except Exception:
        pass
    return False


async def _consent_tick_scopes(target: Any) -> bool:
    clicked = False
    try:
        for kw in ("Select all", "Pilih semua", "Select All"):
            loc = target.get_by_role("button", name=re.compile(rf"^{re.escape(kw)}$", re.I))
            if await loc.count() > 0 and await loc.first.is_visible():
                if await _hard_click_locator(target, loc):
                    clicked = True
                    await asyncio.sleep(0.35)
                    break
    except Exception:
        pass
    try:
        n = await target.evaluate(
            """() => {
                let n = 0;
                const boxes = document.querySelectorAll(
                    'input[type="checkbox"], [role="checkbox"]'
                );
                for (const el of boxes) {
                    const rect = el.getBoundingClientRect();
                    if (rect.width <= 0 || rect.height <= 0) continue;
                    const aria = (el.getAttribute('aria-checked') || '').toLowerCase();
                    const checked = el.checked === true || aria === 'true';
                    if (checked) continue;
                    try { el.click(); n += 1; } catch (_) {}
                }
                return n;
            }"""
        )
        if isinstance(n, int) and n > 0:
            clicked = True
            await asyncio.sleep(0.25)
    except Exception:
        pass
    return clicked


async def _dump_consent_buttons(target: Any) -> str:
    try:
        rows = await target.evaluate(
            """() => {
                const out = [];
                const nodes = document.querySelectorAll(
                  'button, div[role="button"], span[role="button"], a[role="button"], input[type="submit"], input[type="button"]'
                );
                for (const b of nodes) {
                  const raw = (b.innerText || b.textContent || b.value || '').replace(/\\s+/g, ' ').trim();
                  if (!raw || raw.length > 48) continue;
                  const rect = b.getBoundingClientRect();
                  if (rect.width < 20 || rect.height < 12) continue;
                  out.push({
                    t: raw.slice(0, 40),
                    dis: !!(b.disabled || b.getAttribute('aria-disabled') === 'true'),
                    x: Math.round(rect.x + rect.width / 2),
                    y: Math.round(rect.y + rect.height / 2),
                    js: b.getAttribute('jsname') || '',
                    id: b.id || '',
                  });
                  if (out.length >= 12) break;
                }
                return out;
            }"""
        )
        if not rows:
            return "no-buttons"
        parts = []
        for r in rows:
            if not isinstance(r, dict):
                continue
            parts.append(
                f"{r.get('t','?')}@({r.get('x')},{r.get('y')})"
                f"{'|dis' if r.get('dis') else ''}"
                f"{'|js='+r['js'] if r.get('js') else ''}"
            )
        return "; ".join(parts) if parts else "no-buttons"
    except Exception as exc:
        return f"dump-err:{exc}"


async def _find_consent_primary_point(target: Any) -> dict[str, Any] | None:
    try:
        return await target.evaluate(
            """(args) => {
                const positives = args.positives;
                const negatives = args.negatives;
                function collect(root, out) {
                  if (!root) return;
                  const sel = 'button, div[role="button"], span[role="button"], a[role="button"], input[type="submit"], input[type="button"]';
                  try {
                    for (const b of root.querySelectorAll(sel)) out.push(b);
                  } catch (_) {}
                  let all;
                  try { all = root.querySelectorAll('*'); } catch (_) { return; }
                  for (const el of all) {
                    if (el.shadowRoot) collect(el.shadowRoot, out);
                  }
                }
                const nodes = [];
                collect(document, nodes);
                try {
                  const sub = document.querySelector('#submit_approve_access');
                  if (sub) nodes.push(sub, ...sub.querySelectorAll('button, [role="button"]'));
                } catch (_) {}
                const cands = [];
                const seen = new Set();
                for (const b of nodes) {
                  if (!b || seen.has(b)) continue;
                  seen.add(b);
                  const raw = (b.innerText || b.textContent || b.value || b.getAttribute('aria-label') || '')
                    .replace(/\\s+/g, ' ').trim();
                  if (!raw || raw.length > 48) continue;
                  if (b.disabled || b.getAttribute('aria-disabled') === 'true') continue;
                  const rect = b.getBoundingClientRect();
                  if (rect.width < 28 || rect.height < 14) continue;
                  if (rect.bottom < 0 || rect.top > (window.innerHeight || 900) + 40) continue;
                  const low = raw.toLowerCase();
                  if (negatives.some(n => low === n || low.startsWith(n + ' ') || low.includes(' ' + n))) continue;
                  const exact = positives.some(p => low === p);
                  const soft = !exact && positives.some(p =>
                    low.startsWith(p + ' ') || low.endsWith(' ' + p) ||
                    new RegExp('(?:^|\\\\b)' + p.replace(/[.*+?^${}()|[\\]\\\\]/g, '\\\\$&') + '(?:\\\\b|$)').test(low)
                  );
                  const idHit = (b.id || '').toLowerCase().includes('submit_approve')
                    || (b.getAttribute('jsname') || '') === 'LgbsSe';
                  if (!exact && !soft && !idHit) continue;
                  const area = rect.width * rect.height;
                  const bottomBias = rect.top + rect.height;
                  const rightBias = rect.left + rect.width;
                  let score = (exact ? 2e9 : soft ? 1e9 : 5e8) + bottomBias * 20 + rightBias * 5 + area;
                  try {
                    const st = window.getComputedStyle(b);
                    const bg = (st.backgroundColor || '').replace(/\\s/g, '');
                    if (bg && bg !== 'rgba(0,0,0,0)' && bg !== 'transparent' && bg !== 'rgb(255,255,255)') {
                      score += 5e7;
                    }
                  } catch (_) {}
                  cands.push({
                    x: rect.x + rect.width / 2,
                    y: rect.y + rect.height / 2,
                    txt: raw,
                    score,
                  });
                }
                if (!cands.length) return null;
                cands.sort((a, b) => b.score - a.score);
                return cands[0];
            }""",
            {
                "positives": list(_CONSENT_PRIMARY_LABELS),
                "negatives": list(_CONSENT_NEGATIVE_LABELS),
            },
        )
    except Exception:
        return None


async def _activate_consent_hit(
    target: Any, page_for_mouse: Any, hit: dict[str, Any]
) -> bool:
    x = float(hit["x"])
    y = float(hit["y"])
    try:
        if await _straight_move_click(page_for_mouse, x, y):
            await asyncio.sleep(0.12)
            try:
                await page_for_mouse.keyboard.press("Enter")
            except Exception:
                pass
            return True
    except Exception:
        pass
    try:
        ok = await target.evaluate(
            """(p) => {
                const x = p.x, y = p.y;
                let el = document.elementFromPoint(x, y);
                if (!el) return false;
                const btn = el.closest('button, [role="button"], input[type="submit"], a, div');
                const t = btn || el;
                try { t.focus && t.focus(); } catch (_) {}
                for (const type of ['pointerdown','mousedown','pointerup','mouseup','click']) {
                    t.dispatchEvent(new MouseEvent(type, {
                        bubbles: true, cancelable: true, view: window,
                        clientX: x, clientY: y, buttons: 1
                    }));
                }
                if (typeof t.click === 'function') t.click();
                return true;
            }""",
            {"x": x, "y": y},
        )
        if ok:
            try:
                await page_for_mouse.keyboard.press("Enter")
            except Exception:
                pass
            return True
    except Exception:
        pass
    return False


async def _click_consent_primary_on_target(target: Any, page_for_mouse: Any) -> bool:
    mouse_host = page_for_mouse

    for selector in (
        "#submit_approve_access button:not([disabled])",
        "#submit_approve_access",
        "button#submit_approve_access",
        "#confirmNext button",
        'button[data-mdc-dialog-action="ok"]',
        'button[jsname="LgbsSe"]',
    ):
        try:
            loc = target.locator(selector)
            n = await loc.count()
            for i in range(n):
                first = loc.nth(i)
                try:
                    if not await first.is_visible():
                        continue
                except Exception:
                    continue
                try:
                    if await first.is_disabled():
                        continue
                except Exception:
                    pass
                label = ""
                try:
                    label = (await first.inner_text() or "").strip().lower()
                except Exception:
                    label = ""
                if label and any(
                    nlab == label or nlab in label for nlab in _CONSENT_NEGATIVE_LABELS
                ):
                    continue
                if label and not any(
                    p == label or label.startswith(p) for p in _CONSENT_PRIMARY_LABELS
                ):
                    if "lgbsse" in selector.lower() and n > 1:
                        continue
                if await _hard_click_locator(mouse_host, first):
                    try:
                        await mouse_host.keyboard.press("Enter")
                    except Exception:
                        pass
                    return True
        except Exception:
            continue

    for kw in (
        "Continue",
        "Allow",
        "I understand",
        "I agree",
        "Accept",
        "Confirm",
        "Lanjut",
        "Izinkan",
        "Setuju",
        "Mengerti",
    ):
        try:
            loc = target.get_by_role(
                "button", name=re.compile(rf"^\s*{re.escape(kw)}\s*$", re.I)
            )
            n = await loc.count()
            for i in range(n):
                btn = loc.nth(i)
                try:
                    if not await btn.is_visible():
                        continue
                    if await btn.is_disabled():
                        continue
                except Exception:
                    pass
                if await _hard_click_locator(mouse_host, btn):
                    try:
                        await mouse_host.keyboard.press("Enter")
                    except Exception:
                        pass
                    return True
        except Exception:
            pass
        try:
            loc = target.get_by_text(kw, exact=True)
            n = await loc.count()
            for i in range(min(n, 6)):
                el = loc.nth(i)
                try:
                    if not await el.is_visible():
                        continue
                except Exception:
                    continue
                try:
                    box = await el.bounding_box()
                except Exception:
                    box = None
                if box and box.get("width", 0) > 1:
                    try:
                        parent_btn = el.locator(
                            'xpath=ancestor-or-self::button[1] | ancestor-or-self::*[@role="button"][1]'
                        )
                        if await parent_btn.count() > 0:
                            if await _hard_click_locator(mouse_host, parent_btn.first):
                                try:
                                    await mouse_host.keyboard.press("Enter")
                                except Exception:
                                    pass
                                return True
                    except Exception:
                        pass
                    x = box["x"] + box["width"] * 0.5
                    y = box["y"] + box["height"] * 0.5
                    if await _straight_move_click(mouse_host, x, y):
                        try:
                            await mouse_host.keyboard.press("Enter")
                        except Exception:
                            pass
                        return True
        except Exception:
            pass
        try:
            loc = target.locator(
                f'button:text-is("{kw}"), div[role="button"]:text-is("{kw}"), '
                f'span[role="button"]:text-is("{kw}"), input[type="submit"][value="{kw}"]'
            )
            if await loc.count() > 0 and await loc.first.is_visible():
                if await _hard_click_locator(mouse_host, loc):
                    try:
                        await mouse_host.keyboard.press("Enter")
                    except Exception:
                        pass
                    return True
        except Exception:
            pass

    hit = await _find_consent_primary_point(target)
    if hit and isinstance(hit, dict) and "x" in hit and "y" in hit:
        if await _activate_consent_hit(target, mouse_host, hit):
            return True

    try:
        for _ in range(8):
            await mouse_host.keyboard.press("Tab")
            await asyncio.sleep(0.05)
            focused = await target.evaluate(
                """() => {
                    const t = document.activeElement;
                    if (!t) return null;
                    const raw = (t.innerText || t.textContent || t.value || t.getAttribute('aria-label') || '')
                      .replace(/\\s+/g, ' ').trim().toLowerCase();
                    return raw.slice(0, 40);
                }"""
            )
            fl = str(focused or "").lower()
            if fl and any(p == fl or fl.startswith(p) for p in _CONSENT_PRIMARY_LABELS):
                if not any(n in fl for n in _CONSENT_NEGATIVE_LABELS):
                    await mouse_host.keyboard.press("Enter")
                    return True
    except Exception:
        pass
    return False


async def _async_list_consent_targets(page: Any) -> list[Any]:
    out: list[Any] = [page]
    try:
        for fr in page.frames:
            if fr is page.main_frame:
                continue
            try:
                url = (fr.url or "").lower()
            except Exception:
                url = ""
            if (
                "google." in url
                or "accounts.google" in url
                or not url
                or url.startswith("about:")
            ):
                out.append(fr)
    except Exception:
        pass
    return out


async def _click_consent_primary(page: Any) -> bool:
    for target in await _async_list_consent_targets(page):
        try:
            await _consent_tick_scopes(target)
        except Exception:
            pass
        if await _click_consent_primary_on_target(target, page):
            return True
    return False


async def _handle_google_consent_continue(page: Any) -> bool:
    try:
        current_url = page.url
    except Exception:
        current_url = ""
    if "accounts.google.com" not in current_url and "google.com" not in current_url:
        return False

    if not await _looks_like_google_consent_ui(page):
        try:
            if await page.locator(
                "#submit_approve_access, #submit_approve_access button"
            ).count() == 0:
                body = await _page_text_lower(page)
                if (
                    "sign in to" not in body
                    and "google will allow" not in body
                    and "wants to access your google account" not in body
                ):
                    return False
        except Exception:
            return False

    return await _click_consent_primary(page)


async def _handle_google_something_wrong(page: Any) -> bool:
    try:
        current_url = page.url
    except Exception:
        current_url = ""
    if "accounts.google.com" not in current_url:
        return False
    try:
        body = await page.evaluate(
            "() => (document.body && document.body.innerText || '').toLowerCase()"
        )
        body = str(body or "").lower()
    except Exception:
        return False
    if "something went wrong" not in body and "sorry, something went wrong" not in body:
        return False
    if await _human_click_by_text(
        page, ["Try again", "Try Again", "Next", "Coba lagi", "Berikutnya"]
    ):
        return True
    return await _click_google_next(page)


async def click_qoder_google_button(page: Any, prog: Progress, email: str) -> None:
    try:
        try:
            await page.wait_for_load_state("domcontentloaded", timeout=20_000)
        except Exception:
            pass
        try:
            await recover_page_load_error(page)
        except Exception:
            pass
        google_btn = page.locator(
            'a[href*="/sso/login/google"], '
            'a:has-text("Google"), '
            'button:has-text("Google"), '
            'li:has-text("Sign in with Google")'
        ).first
        try:
            await google_btn.wait_for(state="visible", timeout=15_000)
        except Exception:
            pass
        await asyncio.sleep(0.5)
        if await google_btn.count() > 0 and await google_btn.is_visible():
            if await _human_click(page, google_btn):
                try:
                    await page.wait_for_load_state("domcontentloaded", timeout=20_000)
                except Exception:
                    pass
                await asyncio.sleep(1.2)
                prog.log("clicked Google sign-in", "DBG", email=email)
                return
    except Exception as exc:
        prog.log(f"google button click err: {exc}", "DBG", email=email)

    await page.goto(
        "https://qoder.com/sso/login/google?oauth_callback=https://qoder.com/account/profile",
        wait_until="domcontentloaded",
        timeout=30_000,
    )
    try:
        await page.wait_for_load_state("domcontentloaded", timeout=15_000)
    except Exception:
        pass
    await asyncio.sleep(1.0)


async def _wait_after_consent_click(page: Any, url_before: str) -> bool:
    try:
        await page.wait_for_load_state("domcontentloaded", timeout=8_000)
    except Exception:
        pass
    body_before = await _page_text_lower(page)
    deadline = time.monotonic() + 5.0
    advanced = False
    while time.monotonic() < deadline:
        try:
            cur = page.url
        except Exception:
            break
        if cur != url_before:
            advanced = True
            break
        if not await _looks_like_google_consent_ui(page):
            advanced = True
            break
        body_now = await _page_text_lower(page)
        if body_now and body_before and body_now[:200] != body_before[:200]:
            advanced = True
            break
        await asyncio.sleep(0.25)
    await asyncio.sleep(0.45)
    if advanced:
        return True
    try:
        if urlparse(page.url).netloc.endswith("qoder.com"):
            return True
        if not await _looks_like_google_consent_ui(page):
            return True
    except Exception:
        pass
    return False


def is_sso_retryable_error(exc: BaseException) -> bool:
    msg = str(exc).lower()
    needles = (
        "google consent screen stuck",
        "google email step stuck",
        "google auth loop exhausted",
        "google auth stuck no progress",
        "google consent clicked >",
        "google password step stuck",
        "without return to qoder.com",
        "continue/allow not actionable",
    )
    return any(n in msg for n in needles)


async def _sso_recovery_nudge(page: Any) -> None:
    try:
        await recover_page_load_error(page)
    except Exception:
        pass
    try:
        await page.keyboard.press("Escape")
    except Exception:
        pass
    await asyncio.sleep(0.2)
    try:
        await page.keyboard.press("Enter")
    except Exception:
        pass
    await asyncio.sleep(0.35)
    try:
        await _handle_google_consent_continue(page)
    except Exception:
        pass
    try:
        await page.reload(wait_until="domcontentloaded", timeout=20_000)
    except Exception:
        pass
    await asyncio.sleep(1.0)
    try:
        await recover_page_load_error(page)
    except Exception:
        pass


async def drive_google_auth(
    page: Any, email: str, password: str, prog: Progress
) -> None:
    email_transition_deadline = 0.0
    password_transition_deadline = 0.0
    email_step_started_at: float | None = None
    password_step_started_at: float | None = None
    consent_clicks = 0
    consent_attempts = 0
    consent_stuck_since: float | None = None
    last_consent_url = ""
    max_consent_clicks = 8
    last_progress_at = time.monotonic()
    recovery_used = 0
    max_recoveries = 2

    def _mark_progress() -> None:
        nonlocal last_progress_at, consent_stuck_since
        last_progress_at = time.monotonic()
        consent_stuck_since = None

    for tick in range(200):
        try:
            cur = page.url
        except Exception:
            return

        try:
            cur_host = urlparse(cur).netloc
        except Exception:
            cur_host = ""

        if cur_host.endswith("qoder.com"):
            if consent_clicks:
                prog.log(
                    f"redirected back to Qoder after {consent_clicks} consent click(s)",
                    "OK",
                    email=email,
                )
            else:
                prog.log("redirected back to Qoder", "OK", email=email)
            return

        now = time.monotonic()
        idle_s = now - last_progress_at

        if idle_s > 28.0 and tick % 4 == 0:
            snippet = (await _page_text_lower(page))[:120].replace("\n", " ")
            dump = await _dump_consent_buttons(page)
            prog.log(
                f"sso idle {idle_s:.0f}s url={cur[:100]} body≈{snippet!r} btns=[{dump}]",
                "WAIT",
                email=email,
            )

        if idle_s > 35.0 and recovery_used < max_recoveries:
            recovery_used += 1
            prog.log(
                f"sso stuck recovery {recovery_used}/{max_recoveries} (nudge+reload)",
                "WARN",
                email=email,
            )
            await _sso_recovery_nudge(page)
            _mark_progress()
            continue

        if idle_s > 55.0:
            dump = await _dump_consent_buttons(page)
            raise RuntimeError(
                f"google auth stuck no progress >{idle_s:.0f}s "
                f"url={cur[:120]} btns=[{dump}]"
            )

        if await _handle_google_gaplustos(page):
            prog.log("clicked gaplustos / speedbump", "OK", email=email)
            _mark_progress()
            await asyncio.sleep(1.0)
            continue

        if await _handle_google_something_wrong(page):
            prog.log("google error interstitial → Try again/Next (mouse)", "WARN", email=email)
            _mark_progress()
            await asyncio.sleep(1.5)
            continue

        if await _handle_workspace_welcome(page):
            prog.log("clicked Workspace welcome / I understand (mouse)", "OK", email=email)
            _mark_progress()
            await asyncio.sleep(1.2)
            try:
                await page.wait_for_load_state("domcontentloaded", timeout=8_000)
            except Exception:
                pass
            await asyncio.sleep(0.6)
            if await _handle_google_consent_continue(page):
                consent_attempts += 1
                advanced = await _wait_after_consent_click(page, cur)
                if advanced:
                    consent_clicks += 1
                    _mark_progress()
                    prog.log(
                        f"clicked Google consent #{consent_clicks} (post-workspace)",
                        "OK",
                        email=email,
                    )
            continue

        on_consent = await _looks_like_google_consent_ui(page)
        if on_consent or "accounts.google.com" in cur:
            if await _handle_google_consent_continue(page):
                consent_attempts += 1
                advanced = await _wait_after_consent_click(page, cur)
                if advanced:
                    consent_clicks += 1
                    last_consent_url = ""
                    _mark_progress()
                    prog.log(
                        f"clicked Google consent #{consent_clicks} (Continue/Allow)",
                        "OK",
                        email=email,
                    )
                    if consent_clicks > max_consent_clicks:
                        raise RuntimeError(
                            f"google consent clicked >{max_consent_clicks} times without leaving OAuth"
                        )
                else:
                    dump = await _dump_consent_buttons(page)
                    prog.log(
                        f"consent click attempt #{consent_attempts} did not advance UI btns=[{dump}]",
                        "WAIT",
                        email=email,
                    )
                    if consent_stuck_since is None:
                        consent_stuck_since = now
                    if consent_attempts >= 3 and recovery_used < max_recoveries:
                        recovery_used += 1
                        prog.log(
                            f"consent no-advance recovery {recovery_used}/{max_recoveries}",
                            "WARN",
                            email=email,
                        )
                        await _sso_recovery_nudge(page)
                        _mark_progress()
                    await asyncio.sleep(0.85)
                continue

        if on_consent:
            if consent_stuck_since is None or cur != last_consent_url:
                consent_stuck_since = now
                last_consent_url = cur
                snippet = (await _page_text_lower(page))[:140].replace("\n", " ")
                dump = await _dump_consent_buttons(page)
                prog.log(
                    f"consent UI still present (#{consent_clicks} done) url={cur[:90]} "
                    f"body≈{snippet!r} btns=[{dump}]",
                    "WAIT",
                    email=email,
                )
            elif now - consent_stuck_since > 25.0 and recovery_used < max_recoveries:
                recovery_used += 1
                prog.log(
                    f"consent stuck recovery {recovery_used}/{max_recoveries}",
                    "WARN",
                    email=email,
                )
                await _sso_recovery_nudge(page)
                consent_stuck_since = time.monotonic()
                _mark_progress()
                continue
            elif now - consent_stuck_since > 40.0:
                dump = await _dump_consent_buttons(page)
                raise RuntimeError(
                    "google consent screen stuck >40s "
                    f"(Continue/Allow not actionable) btns=[{dump}]"
                )
            await asyncio.sleep(0.7)
            continue

        consent_stuck_since = None

        on_google = cur_host.endswith("accounts.google.com")
        if on_google:
            at_password = await _is_password_step(page)
            at_email = await _is_email_step(page)

            if at_email and not at_password:
                if email_step_started_at is None:
                    email_step_started_at = now
                elif now - email_step_started_at > 60.0:
                    raise RuntimeError(
                        "google email step stuck >60s (captcha/block suspected)"
                    )
                if now < email_transition_deadline:
                    await asyncio.sleep(0.5)
                    continue
                if await _fill_google_email_step(page, email):
                    email_transition_deadline = time.monotonic() + 8.0
                    _mark_progress()
                    await asyncio.sleep(1.0)
                    continue
                await asyncio.sleep(0.8)
                continue

            if at_password:
                email_step_started_at = None
                if password_step_started_at is None:
                    password_step_started_at = now
                elif now - password_step_started_at > 60.0:
                    raise RuntimeError(
                        "google password step stuck >60s (wrong password or block)"
                    )
                if now < password_transition_deadline:
                    await asyncio.sleep(0.5)
                    continue
                if await _fill_google_password_step(page, password):
                    password_transition_deadline = time.monotonic() + 10.0
                    password_step_started_at = None
                    _mark_progress()
                    await asyncio.sleep(1.0)
                    continue
                await asyncio.sleep(0.8)
                continue

            password_step_started_at = None
            if tick % 3 == 0:
                prog.log(
                    f"google interstitial (no email/password/consent match) url={cur[:100]}",
                    "WAIT",
                    email=email,
                )
            if await _handle_google_consent_continue(page):
                consent_attempts += 1
                advanced = await _wait_after_consent_click(page, cur)
                if advanced:
                    consent_clicks += 1
                    _mark_progress()
                    prog.log(
                        f"clicked Google consent #{consent_clicks} (fallback)",
                        "OK",
                        email=email,
                    )
                else:
                    prog.log(
                        f"consent fallback attempt #{consent_attempts} no advance",
                        "WAIT",
                        email=email,
                    )
                continue
            await asyncio.sleep(0.9)
            continue

        email_step_started_at = None
        password_step_started_at = None
        if tick % 5 == 0:
            prog.log(
                f"sso waiting (host={cur_host or '?'}) url={cur[:100]}",
                "WAIT",
                email=email,
            )
        await asyncio.sleep(1.0)

    raise RuntimeError("google auth loop exhausted without return to qoder.com")
