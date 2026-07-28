from __future__ import annotations

import asyncio
import json
import random
import re
import time
from typing import Any

from .config import Config
from .progress import Progress


def _mask_key(key: str) -> str:
    k = (key or "").strip()
    if not k:
        return "(empty)"
    if len(k) <= 10:
        return "****"
    return f"{k[:8]}…{k[-4:]} (len={len(k)})"


async def _fill_input(page: Any, selector: str, value: str) -> bool:
    """Fill a single visible input by CSS selector (id/name preferred)."""
    for sel in (
        selector,
        f"input{selector}" if selector.startswith("#") or selector.startswith("[") else selector,
    ):
        try:
            loc = page.locator(sel).first
            if await loc.count() == 0:
                continue
            if not await loc.is_visible():
                # still try force-fill — some UIs hide until CF clears
                pass
            await loc.click(force=True, timeout=5_000)
            await asyncio.sleep(0.1)
            try:
                await loc.fill("")
            except Exception:
                pass
            await loc.fill(value, timeout=5_000)
            await asyncio.sleep(0.15)
            # verify value stuck
            try:
                got = await loc.input_value(timeout=2_000)
                if got == value:
                    return True
            except Exception:
                return True
            return True
        except Exception:
            continue
    return False


async def _fill_key_and_pat(page: Any, pat: str, access_key: str) -> tuple[bool, bool]:
    """
    Fill dudul inject form:
      <input id="key" name="key" …>
      <input id="pat" name="pat" …>
    Returns (key_ok, pat_ok).
    """
    key_ok = False
    pat_ok = False

    # Prefer exact ids from live page
    key_selectors = [
        "#key",
        'input#key',
        'input[name="key"]',
        'input[placeholder*="dudul" i]',
        'input[placeholder*="access" i]',
    ]
    pat_selectors = [
        "#pat",
        'input#pat',
        'input[name="pat"]',
        'input[placeholder*="pt-" i]',
        'textarea[name="pat"]',
        'input[placeholder*="token" i]',
        'textarea[placeholder*="token" i]',
        "textarea",
    ]

    for sel in key_selectors:
        if await _fill_input(page, sel, access_key):
            key_ok = True
            break

    for sel in pat_selectors:
        if await _fill_input(page, sel, pat):
            pat_ok = True
            break

    # JS fallback: map by id/name/placeholder
    if not key_ok or not pat_ok:
        try:
            result = await page.evaluate(
                """({ pat, key }) => {
                    const setVal = (el, v) => {
                        if (!el) return false;
                        el.focus();
                        el.value = v;
                        el.dispatchEvent(new Event('input', { bubbles: true }));
                        el.dispatchEvent(new Event('change', { bubbles: true }));
                        return true;
                    };
                    const byId = (id) => document.getElementById(id);
                    const byName = (n) => document.querySelector(`[name="${n}"]`);
                    let keyOk = false;
                    let patOk = false;
                    keyOk = setVal(byId('key') || byName('key'), key) || keyOk;
                    patOk = setVal(byId('pat') || byName('pat'), pat) || patOk;
                    if (!keyOk) {
                        for (const el of document.querySelectorAll('input, textarea')) {
                            if (el.offsetParent === null) continue;
                            const ph = (el.placeholder || '').toLowerCase();
                            const nm = (el.name || el.id || '').toLowerCase();
                            if (ph.includes('dudul') || nm.includes('key') || nm.includes('access')) {
                                keyOk = setVal(el, key);
                                if (keyOk) break;
                            }
                        }
                    }
                    if (!patOk) {
                        for (const el of document.querySelectorAll('input, textarea')) {
                            if (el.offsetParent === null) continue;
                            const ph = (el.placeholder || '').toLowerCase();
                            const nm = (el.name || el.id || '').toLowerCase();
                            if (ph.includes('pt-') || nm === 'pat' || ph.includes('token') || nm.includes('token')) {
                                patOk = setVal(el, pat);
                                if (patOk) break;
                            }
                        }
                    }
                    return { keyOk, patOk };
                }""",
                {"pat": pat, "key": access_key},
            )
            if isinstance(result, dict):
                key_ok = key_ok or bool(result.get("keyOk"))
                pat_ok = pat_ok or bool(result.get("patOk"))
        except Exception:
            pass

    return key_ok, pat_ok


async def _try_click_submit(page: Any) -> bool:
    keywords = (
        "inject",
        "submit",
        "start",
        "run",
        "go",
        "claim",
        "apply",
        "kirim",
        "lanjut",
    )
    try:
        return bool(
            await page.evaluate(
                """(keywords) => {
                    const els = [...document.querySelectorAll('button, input[type="submit"], a[role="button"], div[role="button"]')];
                    for (const el of els) {
                        if (el.offsetParent === null) continue;
                        const txt = ((el.textContent || el.value || '') + '').trim().toLowerCase();
                        if (!txt) continue;
                        if (keywords.some(k => txt.includes(k))) {
                            el.click();
                            return true;
                        }
                    }
                    const primary = document.querySelector('button[type="submit"], form button, form input[type="submit"]');
                    if (primary && primary.offsetParent !== null) {
                        primary.click();
                        return true;
                    }
                    return false;
                }""",
                list(keywords),
            )
        )
    except Exception:
        return False


_TURNSTILE_SELECTORS = (
    ".cf-turnstile",
    "#cf-turnstile",
    'iframe[src*="turnstile"]',
    'iframe[src*="challenges.cloudflare"]',
    "[data-sitekey]",
)


async def _turnstile_present(page: Any) -> bool:
    """True only if a Turnstile / CF challenge widget is actually on the page."""
    try:
        for frame in page.frames:
            url = (frame.url or "").lower()
            if "challenges.cloudflare.com" in url or "turnstile" in url:
                return True
    except Exception:
        pass
    for sel in _TURNSTILE_SELECTORS:
        try:
            loc = page.locator(sel).first
            if await loc.count() > 0:
                return True
        except Exception:
            continue
    return False


async def _jiggle_mouse_toward(page: Any, x: float, y: float) -> None:
    try:
        cur = await page.evaluate(
            "() => ({ x: window.innerWidth * 0.3, y: window.innerHeight * 0.4 })"
        )
        sx = float(cur.get("x", 100)) if isinstance(cur, dict) else 100.0
        sy = float(cur.get("y", 200)) if isinstance(cur, dict) else 200.0
        for _ in range(random.randint(4, 7)):
            mx = sx + (x - sx) * random.uniform(0.15, 0.55) + random.uniform(-18, 18)
            my = sy + (y - sy) * random.uniform(0.15, 0.55) + random.uniform(-14, 14)
            await page.mouse.move(mx, my, steps=random.randint(6, 14))
            await asyncio.sleep(random.uniform(0.04, 0.11))
            sx, sy = mx, my
        await page.mouse.move(
            x + random.uniform(-3, 3),
            y + random.uniform(-3, 3),
            steps=random.randint(8, 16),
        )
        await asyncio.sleep(random.uniform(0.08, 0.16))
    except Exception:
        pass


async def _try_click_turnstile(page: Any) -> bool:
    if not await _turnstile_present(page):
        return False

    async def _click_with_jiggle(target: Any) -> bool:
        try:
            box = await target.bounding_box()
        except Exception:
            box = None
        if box and box.get("width", 0) > 1 and box.get("height", 0) > 1:
            cx = box["x"] + box["width"] * random.uniform(0.35, 0.65)
            cy = box["y"] + box["height"] * random.uniform(0.35, 0.65)
            await _jiggle_mouse_toward(page, cx, cy)
            try:
                await page.mouse.down()
                await asyncio.sleep(random.uniform(0.05, 0.12))
                await page.mouse.up()
                return True
            except Exception:
                pass
        try:
            await target.click(timeout=4000, force=True)
            return True
        except Exception:
            return False

    try:
        for frame in page.frames:
            try:
                url = (frame.url or "").lower()
                if "challenges.cloudflare.com" not in url and "turnstile" not in url:
                    continue
                box = frame.locator("input[type='checkbox'], body").first
                if await box.count() > 0:
                    if await _click_with_jiggle(box):
                        return True
            except Exception:
                continue
    except Exception:
        pass
    for sel in _TURNSTILE_SELECTORS:
        try:
            loc = page.locator(sel).first
            if await loc.count() == 0:
                continue
            if await _click_with_jiggle(loc):
                return True
        except Exception:
            continue
    return False


_ACCESS_KEY_EXHAUSTED_HINTS = (
    "no credit left on this key",
    "no credits left on this key",
    "insufficient credit",
    "insufficient credits",
    "key has no credit",
    "key out of credit",
    "access key exhausted",
    "quota exhausted on this key",
    "this key is out of credit",
    "this key has no credit",
)


def _body_has_access_key_exhausted(body: str) -> bool:
    b = (body or "").lower()
    if "no credit left" in b and "key" in b:
        return True
    if "no credits left" in b and "key" in b:
        return True
    return any(h in b for h in _ACCESS_KEY_EXHAUSTED_HINTS)


async def _page_body_text(page: Any) -> str:
    try:
        return await page.locator("body").inner_text(timeout=5000)
    except Exception:
        return ""


def _detect_inject_package(body: str) -> dict[str, Any]:
    low = (body or "").lower()
    package: str | None = None
    tier: str | None = None

    if "ultimate" in low:
        package = "ultimate"
        tier = "full"
    elif "pro trial" in low or "pro-trial" in low:
        package = "pro_trial"
        tier = "partial"
    elif re.search(r"\bpartial\b", low):
        package = "pro_trial"
        tier = "partial"

    credits: int | None = None
    m = re.search(r"(\d[\d,]*)\s*credit", low)
    if m:
        try:
            credits = int(m.group(1).replace(",", ""))
        except ValueError:
            credits = None
    if credits is None and package == "pro_trial":
        credits = 300
    if package == "ultimate" and credits is None:
        m2 = re.search(r"(\d[\d,]*)\s*(?:request|call|req)", low)
        if m2:
            try:
                credits = int(m2.group(1).replace(",", ""))
            except ValueError:
                credits = None
        if credits is None:
            credits = 200

    return {
        "package": package,
        "tier": tier,
        "credits_hint": credits,
    }


def parse_dudul_inject_api(data: dict[str, Any] | None) -> dict[str, Any]:
    if not isinstance(data, dict):
        return {"ok": False, "reason": "empty inject api body"}

    success = bool(data.get("success") or data.get("injected"))
    key_left = data.get("key_credits_left")
    try:
        key_left_n = int(key_left) if key_left is not None else None
    except (TypeError, ValueError):
        key_left_n = None

    result = data.get("result") if isinstance(data.get("result"), dict) else {}
    claim = data.get("claim") if isinstance(data.get("claim"), dict) else {}
    umid = data.get("umid") if isinstance(data.get("umid"), dict) else {}

    plan = str(result.get("plan") or "").strip()
    plan_l = plan.lower()
    user_type = result.get("user_type") or result.get("userType")
    trial_granted = bool(result.get("trial_granted"))
    ultimate_claimed = bool(result.get("ultimate_claimed"))
    credits_total = result.get("credits_total")
    credits_remaining = result.get("credits_remaining")

    ultimate_limit = None
    ultimate_remaining = None
    ultimate_activity = None
    credits_rows = claim.get("credits") if isinstance(claim.get("credits"), list) else []
    for row in credits_rows:
        if not isinstance(row, dict):
            continue
        aid = str(row.get("activityId") or row.get("activity_id") or "")
        models = row.get("modelKeys") or []
        if (
            "ultimate" in aid.lower()
            or row.get("type") == "MODEL_FREE_QUOTA"
            or (isinstance(models, list) and any("ultimate" in str(m).lower() for m in models))
        ):
            ultimate_activity = aid or "ultimate"
            try:
                ultimate_limit = int(row.get("limit")) if row.get("limit") is not None else None
            except (TypeError, ValueError):
                ultimate_limit = None
            try:
                ultimate_remaining = (
                    int(row.get("remaining")) if row.get("remaining") is not None else None
                )
            except (TypeError, ValueError):
                ultimate_remaining = None
            break

    package = None
    tier = None
    if "pro trial" in plan_l or trial_granted or (
        isinstance(credits_total, (int, float)) and float(credits_total) >= 300
    ):
        package = "pro_trial"
        tier = "partial"
    if ultimate_claimed or ultimate_activity:
        if package is None:
            package = "ultimate"
            tier = "full"
        elif package == "pro_trial":
            tier = "partial+ultimate"

    credits_hint = None
    try:
        if credits_total is not None:
            credits_hint = int(float(credits_total))
        elif credits_remaining is not None:
            credits_hint = int(float(credits_remaining))
    except (TypeError, ValueError):
        credits_hint = None

    credits_total_n = None
    credits_remaining_n = None
    try:
        if credits_total is not None:
            credits_total_n = int(float(credits_total))
        if credits_remaining is not None:
            credits_remaining_n = int(float(credits_remaining))
    except (TypeError, ValueError):
        pass

    has_pro_trial = (
        package == "pro_trial"
        or trial_granted
        or "pro trial" in plan_l
        or (credits_total_n is not None and credits_total_n > 0)
        or (credits_remaining_n is not None and credits_remaining_n > 0)
    )
    has_ultimate = bool(ultimate_claimed or ultimate_activity)
    granted = has_pro_trial or has_ultimate

    free_plan = plan_l in ("", "free", "free plan", "standard", "personal_standard")
    empty_grant = (
        not has_pro_trial
        and not has_ultimate
        and (credits_total_n is None or credits_total_n <= 0)
        and (credits_remaining_n is None or credits_remaining_n <= 0)
    )

    msg = str(
        data.get("message")
        or data.get("error")
        or data.get("reason")
        or result.get("message")
        or ""
    )
    msg_l = msg.lower()

    exhausted = False
    if key_left_n is not None and key_left_n <= 0 and not success:
        exhausted = True
    if not success and (
        _body_has_access_key_exhausted(msg)
        or _body_has_access_key_exhausted(json.dumps(data, ensure_ascii=False))
    ):
        exhausted = True
    if not success and (
        "no credit left" in msg_l
        or "credit left on this key" in msg_l
        or "key_credits" in msg_l
    ):
        exhausted = True

    out: dict[str, Any] = {
        "ok": False,
        "injected": bool(data.get("injected")),
        "api_success": success,
        "key_credits_left": key_left_n,
        "plan": plan or None,
        "user_type": user_type,
        "trial_granted": trial_granted,
        "ultimate_claimed": ultimate_claimed,
        "credits_total": credits_total,
        "credits_remaining": credits_remaining,
        "package": package,
        "tier": tier,
        "credits_hint": credits_hint,
        "ultimate_limit": ultimate_limit,
        "ultimate_remaining": ultimate_remaining,
        "ultimate_activity": ultimate_activity,
        "uid": result.get("uid"),
        "email": result.get("email"),
        "duration_ms": data.get("duration_ms"),
        "umid": {
            "machineToken": umid.get("machineToken"),
            "machineType": umid.get("machineType"),
            "machineCode": umid.get("machineCode"),
            "vmInfo": umid.get("vmInfo"),
        }
        if umid
        else None,
    }
    if exhausted:
        out["fatal"] = True
        out["fatal_code"] = "dudul_access_key_exhausted"
        out["reason"] = msg or "No credit left on this key."
    elif not success:
        out["reason"] = msg or "inject api success=false"
    elif not granted or (free_plan and empty_grant):
        out["reason"] = (
            f"no credit granted (plan={plan or 'Free'}, "
            f"credits={credits_total_n if credits_total_n is not None else 0}, "
            f"trial={trial_granted}, ultimate={ultimate_claimed})"
        )
        out["detail"] = plan or "Free"
        out["noop"] = True
    else:
        out["ok"] = True
        bits = []
        if plan:
            bits.append(plan)
        if ultimate_claimed or ultimate_activity:
            bits.append(
                f"ultimate {ultimate_remaining or ultimate_limit or 200} free invokes"
            )
        if key_left_n is not None:
            bits.append(f"key_left={key_left_n}")
        out["detail"] = ", ".join(bits) if bits else "inject api success"
    return out


async def _classify_dudul_page(page: Any) -> tuple[str, str, dict[str, Any]]:
    """
    Returns (kind, detail, extra) where kind is one of:
      success | access_key_exhausted | hard_fail | unknown
    extra may include package/tier/credits_hint on success.
    """
    body = await _page_body_text(page)
    low = body.lower()
    empty: dict[str, Any] = {}
    if not low.strip():
        return "unknown", "", empty

    if _body_has_access_key_exhausted(low):
        snippet = body.strip().replace("\n", " ")
        if len(snippet) > 160:
            snippet = snippet[:157] + "…"
        return "access_key_exhausted", snippet or "No credit left on this key.", empty

    hard_fail = (
        "invalid key",
        "wrong key",
        "unauthorized",
        "forbidden",
        "access denied",
        "invalid pat",
        "invalid token",
        "pat invalid",
        "token invalid",
    )
    for h in hard_fail:
        if h in low:
            return "hard_fail", h, empty

    pkg = _detect_inject_package(body)

    success_hints = (
        "success",
        "injected",
        "complete",
        "claimed",
        "berhasil",
        "pro trial",
        "ultimate",
    )
    fail_noise = ("invalid", "failed", "error", "gagal", "blocked")
    has_partial_word = bool(re.search(r"\bpartial\b", low))
    has_success = any(h in low for h in success_hints) or has_partial_word

    if has_success and not any(h in low for h in fail_noise):
        if has_partial_word and not pkg.get("package"):
            pkg = {
                "package": "pro_trial",
                "tier": "partial",
                "credits_hint": 300,
            }
        detail = "partial package (pro trial)" if pkg.get("tier") == "partial" else "matched success text"
        if pkg.get("package") == "ultimate":
            detail = "ultimate package"
        return "success", detail, pkg

    if "200" in low and "inject" in low and not any(h in low for h in fail_noise):
        return "success", "http 200 inject", pkg

    return "unknown", "", empty


async def _page_looks_success(page: Any) -> bool:
    kind, _, _ = await _classify_dudul_page(page)
    return kind == "success"


async def settle_before_inject(cfg: Config, prog: Progress, email: str) -> None:
    secs = max(0, int(cfg.inject_settle_secs))
    if secs <= 0:
        return
    prog.step(email, "settle", f"{secs}s before dudul inject")
    remaining = secs
    while remaining > 0:
        chunk = min(15, remaining)
        await asyncio.sleep(chunk)
        remaining -= chunk
        if remaining > 0:
            prog.log(f"settle {remaining}s left", "WAIT", email=email)


async def dudul_inject(
    page: Any,
    pat: str,
    cfg: Config,
    prog: Progress,
    email: str,
) -> dict[str, Any]:
    """
    Visit dudul.dev/inject, fill access key + PAT, handle Turnstile, submit.

    Live form (2026-07):
      { "pat": "pt-…", "key": "dudul-…" }
      #key  name=key  placeholder=dudul-...
      #pat  name=pat  placeholder=pt-...

    Retry ≤ inject_max_attempts with backoff; total budget inject_total_budget_s.
    """
    if not cfg.dudul_inject:
        return {"ok": False, "skipped": True, "reason": "QODER_DUDUL_INJECT=false"}

    access_key = (cfg.dudul_access_key or "").strip()
    if not access_key:
        return {
            "ok": False,
            "skipped": False,
            "reason": "missing QODER_DUDUL_ACCESS_KEY",
        }

    await settle_before_inject(cfg, prog, email)

    deadline = time.monotonic() + max(30, int(cfg.inject_total_budget_s))
    max_attempts = max(1, int(cfg.inject_max_attempts))
    backoff = list(cfg.inject_backoff) or [2.0] * 7
    last_err = ""

    prog.log(f"dudul key {_mask_key(access_key)}", "INFO", email=email)

    for attempt in range(1, max_attempts + 1):
        if time.monotonic() > deadline:
            last_err = "total budget exhausted"
            break

        prog.step(email, "inject", f"attempt {attempt}/{max_attempts}")
        try:
            resp = await page.goto(
                cfg.dudul_url,
                wait_until="domcontentloaded",
                timeout=45_000,
            )
            status = resp.status if resp else 0
            if status in (403, 503, 429):
                last_err = f"http {status} on navigate (CF challenge?)"
                prog.log(last_err, "WAIT", email=email)
                await asyncio.sleep(2.0)
                if await _turnstile_present(page):
                    prog.log("turnstile after CF status — jiggle click", "WAIT", email=email)
                    if await _try_click_turnstile(page):
                        await asyncio.sleep(2.5)
                try:
                    await page.wait_for_load_state("domcontentloaded", timeout=15_000)
                except Exception:
                    pass

            form_ready = False
            try:
                await page.wait_for_selector(
                    "#key, input[name='key'], #pat, input[name='pat']",
                    timeout=30_000,
                    state="visible",
                )
                form_ready = True
            except Exception:
                await asyncio.sleep(2.0)
                if await _turnstile_present(page):
                    prog.log("form blocked — turnstile jiggle", "WAIT", email=email)
                    await _try_click_turnstile(page)
                    await asyncio.sleep(2.0)
                    try:
                        await page.wait_for_selector(
                            "#key, input[name='key'], #pat, input[name='pat']",
                            timeout=20_000,
                            state="visible",
                        )
                        form_ready = True
                    except Exception:
                        pass

            if not form_ready and status in (403, 503, 429):
                last_err = f"http {status}; form not ready after CF wait"
                prog.log(last_err, "WAIT", email=email)
            else:
                if await _turnstile_present(page):
                    prog.log("turnstile detected — jiggle click", "WAIT", email=email)
                    clicked = await _try_click_turnstile(page)
                    if clicked:
                        await asyncio.sleep(1.5)
                    else:
                        prog.log("turnstile present but click failed", "WAIT", email=email)

                key_ok, pat_ok = await _fill_key_and_pat(page, pat, access_key)
                if not key_ok or not pat_ok:
                    last_err = (
                        f"form fill incomplete key_ok={key_ok} pat_ok={pat_ok} "
                        "(expected #key + #pat)"
                    )
                    prog.log(last_err, "WAIT", email=email)
                else:
                    if await _turnstile_present(page):
                        prog.log("turnstile before submit — jiggle", "WAIT", email=email)
                        await _try_click_turnstile(page)
                        await asyncio.sleep(1.0)

                    api_json: dict[str, Any] | None = None
                    api_status = 0
                    submitted = False
                    try:
                        async with page.expect_response(
                            lambda r: "/api/inject" in (r.url or "")
                            and r.request.method.upper() == "POST",
                            timeout=90_000,
                        ) as resp_info:
                            submitted = await _try_click_submit(page)
                            if not submitted:
                                try:
                                    await page.keyboard.press("Enter")
                                    submitted = True
                                except Exception:
                                    pass
                        api_resp = await resp_info.value
                        api_status = int(getattr(api_resp, "status", 0) or 0)
                        try:
                            api_json = await api_resp.json()
                        except Exception:
                            try:
                                txt = await api_resp.text()
                                api_json = json.loads(txt) if txt else None
                            except Exception:
                                api_json = None
                    except Exception as wait_exc:
                        if not submitted:
                            submitted = await _try_click_submit(page)
                            if not submitted:
                                try:
                                    await page.keyboard.press("Enter")
                                    submitted = True
                                except Exception:
                                    pass
                        await asyncio.sleep(3.5)
                        last_err = f"api/inject wait: {wait_exc}"
                        prog.log(last_err, "WAIT", email=email)

                    if isinstance(api_json, dict):
                        parsed = parse_dudul_inject_api(api_json)
                        if parsed.get("fatal") or parsed.get("fatal_code") == "dudul_access_key_exhausted":
                            reason = parsed.get("reason") or "No credit left on this key."
                            prog.log(
                                f"dudul access key exhausted: {reason}",
                                "ERR",
                                email=email,
                            )
                            return {
                                "ok": False,
                                "skipped": False,
                                "fatal": True,
                                "fatal_code": "dudul_access_key_exhausted",
                                "reason": reason,
                                "key_credits_left": parsed.get("key_credits_left"),
                                "attempt": attempt,
                                "attempts": attempt,
                                "url": cfg.dudul_url,
                                "key_masked": _mask_key(access_key),
                                "api_status": api_status,
                            }
                        if parsed.get("ok"):
                            package = parsed.get("package")
                            tier = parsed.get("tier")
                            credits_hint = parsed.get("credits_hint")
                            detail = parsed.get("detail") or "inject api success"
                            prog.log(
                                f"dudul inject success ({detail})",
                                "OK",
                                email=email,
                            )
                            out: dict[str, Any] = {
                                "ok": True,
                                "attempt": attempt,
                                "url": cfg.dudul_url,
                                "key_masked": _mask_key(access_key),
                                "detail": detail,
                                "api_status": api_status,
                                "key_credits_left": parsed.get("key_credits_left"),
                                "plan": parsed.get("plan"),
                                "user_type": parsed.get("user_type"),
                                "trial_granted": parsed.get("trial_granted"),
                                "ultimate_claimed": parsed.get("ultimate_claimed"),
                                "credits_total": parsed.get("credits_total"),
                                "credits_remaining": parsed.get("credits_remaining"),
                                "ultimate_limit": parsed.get("ultimate_limit"),
                                "ultimate_remaining": parsed.get("ultimate_remaining"),
                                "ultimate_activity": parsed.get("ultimate_activity"),
                                "uid": parsed.get("uid"),
                                "umid": parsed.get("umid"),
                            }
                            if package:
                                out["package"] = package
                            if tier:
                                out["tier"] = tier
                            if credits_hint is not None:
                                out["credits_hint"] = credits_hint
                            return out
                        last_err = parsed.get("reason") or f"inject api http {api_status}"
                        prog.log(last_err, "WAIT", email=email)
                    else:
                        kind, detail, extra = await _classify_dudul_page(page)
                        if kind == "success":
                            package = extra.get("package")
                            tier = extra.get("tier")
                            credits_hint = extra.get("credits_hint")
                            prog.log(
                                f"dudul inject success (page fallback: {detail})",
                                "OK",
                                email=email,
                            )
                            out = {
                                "ok": True,
                                "attempt": attempt,
                                "url": cfg.dudul_url,
                                "key_masked": _mask_key(access_key),
                                "detail": detail,
                            }
                            if package:
                                out["package"] = package
                            if tier:
                                out["tier"] = tier
                            if credits_hint is not None:
                                out["credits_hint"] = credits_hint
                            return out
                        if kind == "access_key_exhausted":
                            reason = detail or "No credit left on this key."
                            prog.log(
                                f"dudul access key exhausted: {reason}",
                                "ERR",
                                email=email,
                            )
                            return {
                                "ok": False,
                                "skipped": False,
                                "fatal": True,
                                "fatal_code": "dudul_access_key_exhausted",
                                "reason": reason,
                                "attempt": attempt,
                                "attempts": attempt,
                                "url": cfg.dudul_url,
                                "key_masked": _mask_key(access_key),
                            }
                        if kind == "hard_fail":
                            last_err = f"dudul hard fail: {detail}"
                            prog.log(last_err, "WAIT", email=email)
                        else:
                            last_err = (
                                "submit done but success not confirmed"
                                if submitted
                                else "submit control not found"
                            )
                            prog.log(last_err, "WAIT", email=email)
        except Exception as exc:
            last_err = str(exc)
            prog.log(f"inject err: {last_err}", "DBG", email=email)

        if attempt < max_attempts:
            delay = backoff[min(attempt - 1, len(backoff) - 1)]
            remain = deadline - time.monotonic()
            if remain <= 0:
                break
            delay = min(delay, max(0.5, remain))
            prog.log(f"inject backoff {delay:.0f}s", "WAIT", email=email)
            await asyncio.sleep(delay)

    prog.log(f"dudul inject failed: {last_err}", "ERR", email=email)
    exhausted = _body_has_access_key_exhausted(last_err)
    return {
        "ok": False,
        "skipped": False,
        "fatal": exhausted,
        "fatal_code": "dudul_access_key_exhausted" if exhausted else None,
        "reason": last_err or "unknown",
        "attempts": max_attempts,
        "url": cfg.dudul_url,
        "key_masked": _mask_key(access_key),
    }
