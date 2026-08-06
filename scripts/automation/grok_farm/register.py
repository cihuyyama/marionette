from __future__ import annotations

import asyncio
import imaplib
import json
import random
import re
import secrets
import string
import threading
import time
from datetime import datetime, timezone
from email import message_from_bytes
from pathlib import Path
from typing import Any

from . import castle
from . import mail_provider
from .activate import activate_grok_if_needed
from .browser import _normalize_proxy_url, _proxy_dict
from .config import Config
from .device_flow import obtain_tokens_via_browser
from .export import append_pending, drop_pending_email, write_backup
from .login import dismiss_cookie_banner
from .oauth import obtain_oidc_tokens
from .progress import Progress

SIGNUP_URL = "https://accounts.x.ai/sign-up"


def generate_email(domain: str, local_len: int = 16) -> str:
    chars = string.ascii_lowercase + string.digits
    local = "".join(secrets.choice(chars) for _ in range(local_len))
    return f"{local}@{domain}"


def generate_plus_email(gmail_base: str) -> str:
    chars = string.ascii_lowercase + string.digits
    tag = "".join(secrets.choice(chars) for _ in range(12))
    base = gmail_base.split("@")[0]
    domain_part = gmail_base.split("@")[1] if "@" in gmail_base else "gmail.com"
    return f"{base}+{tag}@{domain_part}"


# xAI confirmation codes are XXX-XXX alnum ("K35-1QR"), never plain 6 digits.
# Subject seen live: "SpaceXAI confirmation code: FW9-FCG" (code trailing).
_XAI_CODE_RE = re.compile(r"\b([A-Z0-9]{3}-[A-Z0-9]{3})\b", re.I)
_XAI_SUBJ_CODE_RE = re.compile(
    r"(?:xAI\s+confirmation\s+code[:\s]+([A-Z0-9]{3}-[A-Z0-9]{3})"
    r"|^\s*([A-Z0-9]{3}-[A-Z0-9]{3})\s+xAI\s+confirmation)",
    re.I,
)

_claimed_otps: set[str] = set()
_claimed_otps_lock = threading.Lock()


def _is_plausible_otp(code: str) -> bool:
    code = (code or "").upper().strip()
    if not re.fullmatch(r"[A-Z0-9]{3}-[A-Z0-9]{3}", code):
        return False
    left, right = code.split("-", 1)
    # Reject CSS tokens that share the shape: PER-100, EM-16, RGB-255.
    if re.fullmatch(r"[A-Z]+", left) and re.fullmatch(r"\d+", right):
        return False
    if re.fullmatch(r"\d+", left) and re.fullmatch(r"\d+", right):
        return False
    return code not in {"PER-100", "RGB-255", "PX-16", "EM-16", "REM-16", "MS-300", "MS-200"}


def _extract_otp(subject: str, body: str) -> str | None:
    # Subject "xAI confirmation code: XXX-XXX" is authoritative: the code is
    # explicitly labelled, so trust it directly (LTH-963 is valid, not CSS noise).
    m = _XAI_SUBJ_CODE_RE.search(subject or "")
    if m:
        code = m.group(1) or m.group(2)
        if code and re.fullmatch(r"[A-Z0-9]{3}-[A-Z0-9]{3}", code.upper()):
            return code.upper()
    # Bare token in subject: still trusted (subject rarely carries CSS).
    for m in _XAI_CODE_RE.finditer(subject or ""):
        if re.fullmatch(r"[A-Z0-9]{3}-[A-Z0-9]{3}", m.group(1).upper()):
            return m.group(1).upper()
    # Body scan: strip markup, apply CSS-noise plausibility (PER-100, RGB-255...).
    plain = re.sub(r"<style[\s\S]*?</style>", " ", body or "", flags=re.I)
    plain = re.sub(r"<script[\s\S]*?</script>", " ", plain, flags=re.I)
    plain = re.sub(r"<[^>]+>", " ", plain)
    for m in _XAI_CODE_RE.finditer(plain):
        if _is_plausible_otp(m.group(1)):
            return m.group(1).upper()
    return None


def _matches_target(msg: Any, target_email: str) -> bool:
    target = target_email.lower()
    for hdr in ("To", "Delivered-To", "X-Original-To", "Cc"):
        val = (msg.get(hdr) or "").lower()
        if target in val:
            return True
    return False


def _connect_imap(cfg: Config) -> imaplib.IMAP4_SSL | None:
    try:
        mail = imaplib.IMAP4_SSL(cfg.imap_host, cfg.imap_port)
        mail.login(cfg.imap_user, cfg.imap_pass)
        mail.select("INBOX")
        return mail
    except Exception as exc:
        print(f"[register] IMAP connect failed: {exc}", flush=True)
        return None


def read_otp_imap(cfg: Config, target_email: str, timeout: int = 180) -> str | None:
    if not cfg.imap_configured:
        print("[register] IMAP not configured, skipping OTP read", flush=True)
        return None
    deadline = time.time() + timeout
    while time.time() < deadline:
        mail = _connect_imap(cfg)
        if mail is None:
            time.sleep(5)
            continue
        try:
            _, data = mail.search(None, '(FROM "x.ai")')
            ids = (data[0] or b"").split()
            if not ids:
                _, data = mail.search(None, '(SUBJECT "confirmation code")')
                ids = (data[0] or b"").split()
            for num in reversed(ids[-30:]):
                try:
                    _, msg_data = mail.fetch(num, "(RFC822)")
                    raw = msg_data[0][1]
                    msg = message_from_bytes(raw)
                    if not _matches_target(msg, target_email):
                        continue
                    subject = msg.get("Subject") or ""
                    body = ""
                    if msg.is_multipart():
                        for part in msg.walk():
                            ct = part.get_content_type()
                            if ct == "text/plain":
                                body = part.get_payload(decode=True).decode("utf-8", errors="replace")
                                break
                    else:
                        body = msg.get_payload(decode=True).decode("utf-8", errors="replace")
                    code = _extract_otp(subject, body)
                    if code:
                        with _claimed_otps_lock:
                            if code in _claimed_otps:
                                continue
                            _claimed_otps.add(code)
                        return code
                except Exception:
                    continue
        finally:
            try:
                mail.logout()
            except Exception:
                pass
        time.sleep(3)
    return None


async def _fill_email_and_submit(page: Any, email: str, prog: Progress) -> bool:
    email_sels = 'input[name="email"], input[type="email"], input[autocomplete="email"]'
    for _ in range(10):
        try:
            loc = page.locator(email_sels)
            if await loc.count() > 0:
                await loc.first.fill(email)
                await asyncio.sleep(0.3)
                btn = page.get_by_role("button", name=re.compile(r"continue|next|sign up", re.I))
                if await btn.count() > 0:
                    await btn.first.click(force=True)
                    return True
        except Exception:
            pass
        await asyncio.sleep(1)
    return False


async def _left_otp_page(page: Any) -> bool:
    # xAI's input-otp auto-submits once the 6th char lands and jumps straight
    # to the profile step, so "no OTP input anymore" / "profile markers present"
    # both mean the code was accepted — not a failure.
    try:
        body = (await page.evaluate("() => document.body.innerText") or "").lower()
        if "first name" in body or "complete your sign up" in body or "create a password" in body:
            return True
        otp_input = page.locator(
            'input[data-input-otp="true"], input[name="code"], input[autocomplete="one-time-code"]'
        )
        if await otp_input.count() == 0:
            return True
    except Exception:
        pass
    return False


async def _fill_otp(page: Any, code: str, prog: Progress) -> bool:
    code_clean = re.sub(r"[^A-Za-z0-9]", "", code).upper()
    otp_sels = (
        'input[data-input-otp="true"], input[name="code"], '
        'input[autocomplete="one-time-code"]'
    )
    for _ in range(5):
        if await _left_otp_page(page):
            return True
        try:
            otp_input = page.locator(otp_sels)
            if await otp_input.count() > 0:
                await otp_input.first.click()
                # Type char-by-char: input-otp is a controlled component and
                # reliably accepts keystrokes, then may auto-submit on completion.
                await page.keyboard.type(code_clean, delay=80)
                await asyncio.sleep(1.2)
                if await _left_otp_page(page):
                    return True
                btn = page.get_by_role("button", name=re.compile(r"confirm|verify|continue|submit", re.I))
                if await btn.count() > 0:
                    await btn.first.click(force=True)
                    await asyncio.sleep(1.0)
                    return True
            slots = page.locator('input[maxlength="1"]')
            if await slots.count() >= 6:
                await slots.first.click()
                await page.keyboard.type(code_clean, delay=80)
                await asyncio.sleep(1.2)
                if await _left_otp_page(page):
                    return True
                btn = page.get_by_role("button", name=re.compile(r"confirm|verify|continue|submit", re.I))
                if await btn.count() > 0:
                    await btn.first.click(force=True)
                    return True
        except Exception:
            pass
        await asyncio.sleep(1)
    return await _left_otp_page(page)


async def _fill_profile_and_password(page: Any, password: str, prog: Progress) -> bool:
    try:
        first_input = page.locator('input[name="firstName"], input[name="given_name"], input[autocomplete="given-name"]')
        if await first_input.count() > 0:
            await first_input.first.fill("User")
        last_input = page.locator('input[name="lastName"], input[name="family_name"], input[autocomplete="family-name"]')
        if await last_input.count() > 0:
            await last_input.first.fill("Grok")
        pw_input = page.locator('input[type="password"], input[name="password"]')
        if await pw_input.count() > 0:
            await pw_input.first.fill(password)
        await asyncio.sleep(0.5)
        return True
    except Exception:
        return False


async def _turnstile_token_len(page: Any) -> int:
    try:
        return int(
            await page.evaluate(
                "() => { const el = document.querySelector('[name=\"cf-turnstile-response\"]');"
                " return el ? (el.value || '').length : 0; }"
            )
        )
    except Exception:
        return 0


async def _find_cf_frame(page: Any) -> Any:
    for fr in page.frames:
        if "challenges.cloudflare.com" in (fr.url or ""):
            return fr
    return None


async def _handle_turnstile_checkbox(
    page: Any, prog: Progress, email: str = "", password: str = "", max_wait: float = 45
) -> bool:
    # The signup Turnstile iframe renders with an EMPTY src attribute, so CSS
    # selectors (iframe[src*=...]) never match. page.frames exposes the live
    # cross-origin frame; frame_element() gives its box for a humanized click
    # on the checkbox (left-center). Verified live: one click -> token len 709.
    deadline = time.monotonic() + max_wait
    clicks = 0
    while time.monotonic() < deadline:
        if await _turnstile_token_len(page) > 20:
            prog.log("turnstile token ok", "OK", email=email, step="turnstile")
            return True
        cf_frame = await _find_cf_frame(page)
        if cf_frame is None:
            await asyncio.sleep(1.5)
            continue
        if clicks < 6:
            try:
                fe = await cf_frame.frame_element()
                box = await fe.bounding_box()
                if box:
                    x = box["x"] + min(30, box["width"] * 0.1)
                    y = box["y"] + box["height"] / 2
                    prog.log(
                        f"click turnstile @({x:.0f},{y:.0f}) try {clicks + 1}",
                        "WAIT",
                        email=email,
                        step="turnstile",
                    )
                    await page.mouse.move(x - 50, y - 15, steps=8)
                    await asyncio.sleep(random.uniform(0.2, 0.4))
                    await page.mouse.move(x, y, steps=12)
                    await asyncio.sleep(random.uniform(0.15, 0.35))
                    await page.mouse.click(x, y)
                    clicks += 1
            except Exception:
                pass
        for _ in range(4):
            await asyncio.sleep(1.5)
            if await _turnstile_token_len(page) > 20:
                prog.log("turnstile token ok", "OK", email=email, step="turnstile")
                return True
    return False


async def _extract_sso_cookie(page: Any) -> str | None:
    try:
        cookies = await page.context.cookies()
        for c in cookies:
            if c.get("name") == "sso" and str(c.get("value") or "").startswith("eyJ"):
                return c["value"]
    except Exception:
        pass
    return None


async def _poll_sso_cookie(page: Any, prog: Progress, email: str) -> str | None:
    # The 'sso' JWT propagates only via explicit per-domain /set-cookie hops;
    # a plain grok.com visit leaves a fresh signup session logged out (no sso).
    sso = await _extract_sso_cookie(page)
    if sso:
        return sso
    hops = (
        "https://auth.x.ai/set-cookie",
        "https://auth.grokusercontent.com/set-cookie",
        "https://grok.com/",
        "https://accounts.x.ai/",
        "https://accounts.x.ai/sign-in?redirect=grok-com",
        "https://grok.com/",
    )
    for hop in hops:
        try:
            await page.goto(hop, wait_until="domcontentloaded", timeout=30000)
        except Exception:
            pass
        for _ in range(3):
            await asyncio.sleep(1.5)
            sso = await _extract_sso_cookie(page)
            if sso:
                prog.log(
                    f"SSO cookie via {hop.split('//')[-1]}", "DBG", email=email, step="sso_extract"
                )
                return sso
    return None


async def _shot(page: Any, cfg: Config, email: str, tag: str, prog: Progress) -> None:
    try:
        cfg.screenshot_dir.mkdir(parents=True, exist_ok=True)
        safe = email.replace("@", "_at_").replace(".", "_")
        path = cfg.screenshot_dir / f"{safe}_{tag}.png"
        await page.screenshot(path=str(path), full_page=True)
        prog.log(f"screenshot -> {path.name}", "DBG", email=email, step=tag)
    except Exception:
        pass


async def _click_resend(page: Any, prog: Progress, email: str) -> None:
    btn = page.get_by_role("button", name=re.compile(r"resend", re.I))
    try:
        if await btn.count() > 0 and await btn.first.is_visible():
            await btn.first.click(timeout=3000)
            prog.log("clicked resend", "DBG", email=email, step="wait_otp")
    except Exception:
        pass


async def _wait_otp(
    page: Any, cfg: Config, email: str, prog: Progress, mail_session: Any
) -> str | None:
    loop = asyncio.get_event_loop()
    if mail_session is not None:
        waited = 0
        while waited < 180:
            otp = await loop.run_in_executor(None, mail_session.poll_otp, 30)
            if otp:
                return otp
            waited += 30
            await _click_resend(page, prog, email)
        return None
    return await loop.run_in_executor(None, read_otp_imap, cfg, email, 180)


async def register_one(
    page: Any,
    email: str,
    password: str,
    cfg: Config,
    prog: Progress,
    proxy_url: str = "",
    mail_session: Any = None,
) -> dict | None:
    prog.step(email, "signup_open", "Opening sign-up page")
    try:
        await page.goto(SIGNUP_URL, wait_until="load", timeout=45000)
    except Exception:
        try:
            await page.goto(SIGNUP_URL, wait_until="domcontentloaded", timeout=45000)
        except Exception:
            prog.log("goto signup failed", "ERR", email=email, step="signup_open")
            return None
    await asyncio.sleep(3)
    await dismiss_cookie_banner(page)

    prog.step(email, "castle", "Minting Castle token")
    await castle.mint(page, prog, email)

    prog.step(email, "signup_email", "Filling email")
    email_btn = page.get_by_role("button", name=re.compile(r"sign up with email", re.I))
    try:
        if await email_btn.count() > 0:
            await email_btn.first.click(force=True, timeout=5000)
            await asyncio.sleep(1)
    except Exception:
        pass

    if not await _fill_email_and_submit(page, email, prog):
        prog.log("email fill/submit failed", "ERR", email=email, step="signup_email")
        await _shot(page, cfg, email, "fail_signup_email", prog)
        return None

    prog.step(email, "wait_otp", "Waiting for OTP")
    otp = await _wait_otp(page, cfg, email, prog, mail_session)
    if not otp:
        prog.log("OTP timeout (180s)", "ERR", email=email, step="wait_otp")
        return None
    prog.log(f"OTP received: {otp}", "DBG", email=email, step="wait_otp")

    prog.step(email, "confirm_otp", "Confirming OTP")
    if not await _fill_otp(page, otp, prog):
        prog.log("OTP fill failed", "ERR", email=email, step="confirm_otp")
        await _shot(page, cfg, email, "fail_confirm_otp", prog)
        return None
    await asyncio.sleep(2)

    prog.step(email, "profile", "Filling profile + password")
    await _fill_profile_and_password(page, password, prog)

    prog.step(email, "turnstile", "Solving Turnstile")
    solved = await _handle_turnstile_checkbox(
        page, prog, email=email, password=password, max_wait=45
    )
    if not solved:
        prog.log("turnstile not solved", "ERR", email=email, step="turnstile")
        await _shot(page, cfg, email, "fail_turnstile", prog)
        return None

    submit_btn = page.get_by_role("button", name=re.compile(r"complete|sign up|create|submit", re.I))
    try:
        if await submit_btn.count() > 0:
            await submit_btn.first.click(force=True, timeout=5000)
    except Exception:
        pass
    await asyncio.sleep(3)

    prog.step(email, "sso_extract", "Extracting SSO cookie")
    sso_cookie = await _poll_sso_cookie(page, prog, email)
    if not sso_cookie:
        prog.log("SSO cookie not found", "ERR", email=email, step="sso_extract")
        await _shot(page, cfg, email, "fail_sso", prog)
        return None
    prog.log(f"SSO cookie: {sso_cookie[:12]}...", "DBG", email=email, step="sso_extract")

    # New accounts have no Grok principal until they visit grok.com + accept
    # terms. Without it, OAuth consent returns "Failed to generate
    # authentication code / Access denied". Confirm an authenticated session
    # before consent; the principal is provisioned async, so poll grok.com until
    # it is truly signed in rather than assuming after one visit.
    provisioned = await activate_grok_if_needed(page, cfg, prog, email)
    if not provisioned:
        await asyncio.sleep(4.0)
        provisioned = await activate_grok_if_needed(page, cfg, prog, email)
    if not provisioned:
        prog.log(
            "grok.com principal unconfirmed — consent may deny",
            "WARN",
            email=email,
            step="activate",
        )

    # Fresh accounts hit "Failed to generate authentication code / Access denied"
    # on PKCE consent, so approve device flow in the already-signed-in browser
    # first (reference's 1000+-account path); fall back to in-page PKCE.
    prog.step(email, "oauth", "OAuth tokens (device -> PKCE)")
    tokens = await _obtain_tokens_with_retry(page, email, password, cfg, prog, proxy_url)

    base = {
        "email": email,
        "password": password,
        "sso_cookie": sso_cookie,
        "created_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    }
    if not tokens or not tokens.get("access_token"):
        prog.log("OAuth incomplete — account saved as sso_only", "WARN", email=email, step="oauth")
        await _shot(page, cfg, email, "fail_oauth", prog)
        return {**base, "tokens": None, "stage": "sso_only"}

    return {**base, "tokens": tokens, "stage": "tokens"}


async def _obtain_tokens_with_retry(
    page: Any,
    email: str,
    password: str,
    cfg: Config,
    prog: Progress,
    proxy_url: str,
) -> dict | None:
    attempts = max(1, cfg.oauth_retries)
    for attempt in range(1, attempts + 1):
        try:
            tokens = await obtain_tokens_via_browser(
                page, cfg, prog=prog, email=email, password=password, proxy_url=proxy_url
            )
            if tokens and tokens.get("access_token"):
                return tokens
        except Exception as exc:
            prog.log(f"device flow error: {exc}", "WARN", email=email, step="oauth")
        prog.log("device flow miss — falling back to PKCE", "WAIT", email=email, step="oauth")
        try:
            tokens = await obtain_oidc_tokens(
                page, email, password, cfg, prog, email, reprovision=activate_grok_if_needed
            )
            if tokens and tokens.get("access_token"):
                return tokens
        except Exception as exc:
            prog.log(f"OAuth failed (device+PKCE): {exc}", "ERR", email=email, step="oauth")
        if attempt < attempts:
            backoff = min(15.0, 3.0 * attempt)
            prog.log(f"oauth retry {attempt}/{attempts - 1} (backoff {backoff:.0f}s)", "WAIT", email=email, step="oauth")
            await asyncio.sleep(backoff)
            await activate_grok_if_needed(page, cfg, prog, email)
    return None


def _result_to_export(r: dict, cfg: Config) -> dict | None:
    """Map register_one result to write_backup shape; None when no access token."""
    tokens = r.get("tokens") or {}
    if not tokens.get("access_token"):
        return None
    return {
        "ok": True,
        "email": r.get("email", ""),
        "accessToken": tokens["access_token"],
        "refreshToken": tokens.get("refresh_token", ""),
        "idToken": tokens.get("id_token", ""),
        "expiresAt": tokens.get("expires_at", ""),
        "expiresIn": tokens.get("expires_in", 21600),
        "scope": tokens.get("scope", ""),
        "clientId": tokens.get("client_id", cfg.client_id),
        "sso": r.get("sso_cookie", ""),
    }


async def run_register(
    cfg: Config,
    prog: Progress,
    count: int = 1,
    concurrency: int = 1,
    domain: str = "",
    password: str = "",
    proxy_file: str = "",
) -> list[dict]:
    from camoufox.async_api import AsyncCamoufox

    is_plus_trick = domain.startswith("plus:")
    email_source = domain[5:] if is_plus_trick else domain

    use_cf_mail = cfg.mail_mode == "cf" or (
        cfg.mail_mode == "auto" and mail_provider.cf_mail_configured(cfg)
    )
    if cfg.mail_mode == "cf" and not mail_provider.cf_mail_configured(cfg):
        prog.log(
            "GROK_MAIL_MODE=cf but GROK_CF_MAIL_* not configured", "ERR", step="start"
        )
        return []

    if not email_source and not use_cf_mail:
        prog.log("no email source configured (domain or gmail base)", "ERR", step="start")
        return []
    if not password:
        prog.log("no password configured (GROK_PASSWORD)", "ERR", step="start")
        return []

    proxies: list[str] = []
    if proxy_file:
        pf = Path(proxy_file)
        if pf.is_file():
            for line in pf.read_text(encoding="utf-8", errors="replace").splitlines():
                raw = line.strip()
                if not raw or raw.startswith("#"):
                    continue
                norm = _normalize_proxy_url(raw)
                if norm:
                    proxies.append(norm)
                else:
                    prog.log(f"skipping unparseable proxy line: {raw[:60]}", "WARN", step="start")
            if proxies:
                prog.log(f"loaded {len(proxies)} proxy(ies)", "INFO", step="start")

    results: list[dict] = []
    sem = asyncio.Semaphore(max(1, concurrency))
    save_lock = asyncio.Lock()
    proxy_idx = 0

    if use_cf_mail:
        prog.log(
            f"signup mail via temp-mail worker ({cfg.cf_mail_domain})", "INFO", step="start"
        )

    async def worker(idx: int) -> None:
        nonlocal proxy_idx
        async with sem:
            mail_session: Any = None
            if use_cf_mail:
                try:
                    client = mail_provider.create_cf_client(cfg)
                    addr, jwt, addr_id = await asyncio.get_event_loop().run_in_executor(
                        None, client.create_address
                    )
                    email = addr
                    mail_session = mail_provider.TempMailSession(
                        email=addr, jwt=jwt, address_id=addr_id,
                        client=client, extract_otp=_extract_otp,
                    )
                    prog.log(f"temp-mail {addr}", "INFO", step="start")
                except Exception as exc:
                    prog.log(f"temp-mail create failed: {exc}", "ERR", step="start")
                    if cfg.mail_mode == "cf":
                        return
                    email = generate_plus_email(email_source) if is_plus_trick else generate_email(email_source)
            else:
                email = generate_plus_email(email_source) if is_plus_trick else generate_email(email_source)
            proxy_url = ""
            if proxies:
                proxy_url = proxies[proxy_idx % len(proxies)]
                proxy_idx += 1

            humanize_val = cfg.humanize_headed if not cfg.headless else cfg.humanize_headless
            launch_kwargs: dict[str, Any] = {
                "headless": cfg.headless,
                "humanize": humanize_val if cfg.humanize else False,
                "os": cfg.browser_os,
                "block_webrtc": True,
                "locale": "en-US",
                "geoip": True,
                "disable_coop": True,
                "i_know_what_im_doing": True,
            }
            if proxy_url:
                launch_kwargs["proxy"] = _proxy_dict(proxy_url)
                prog.log(f"proxy {proxy_url.split('@')[-1]}", "INFO", email=email, step="launch")

            try:
                prog.step(email, "launch", "starting browser")
                async with AsyncCamoufox(**launch_kwargs) as browser:
                    page = await browser.new_page()
                    prog.step(email, "register", "filling signup form")
                    result = await register_one(page, email, password, cfg, prog, proxy_url, mail_session)
                    if result and result.get("stage") == "tokens":
                        async with save_lock:
                            results.append(result)
                            export_row = _result_to_export(result, cfg)
                            if export_row:
                                try:
                                    n, path = write_backup([export_row], cfg.output, append=True)
                                    drop_pending_email(email, cfg.output)
                                    prog.log(f"saved -> {path} (+{n})", "INFO", email=email)
                                except Exception as exc:
                                    prog.log(f"save err: {exc}", "ERR", email=email)
                        prog.mark_ok(email, "registered + tokens obtained")
                    elif result and result.get("stage") == "sso_only":
                        async with save_lock:
                            results.append(result)
                            try:
                                pp = append_pending(
                                    {"email": email, "password": password, "sso": result.get("sso_cookie", "")},
                                    cfg.output,
                                )
                                prog.log(f"pending (sso_only) -> {pp}", "WARN", email=email)
                            except Exception as exc:
                                prog.log(f"pending write err: {exc}", "ERR", email=email)
                        prog.mark_fail(email, "signup ok but OAuth incomplete (saved to pending)")
                    else:
                        prog.mark_fail(email, "registration flow returned no result")
            except Exception as e:
                err = str(e)
                if "Invalid URL" in err:
                    hint = f"proxy config broken: {proxy_url[:40] if proxy_url else 'none'}"
                elif "Timeout" in err or "timeout" in err:
                    hint = f"timeout during registration (check proxy/network)"
                else:
                    hint = err[:150]
                prog.mark_fail(email, hint)
            finally:
                if mail_session is not None:
                    try:
                        await asyncio.get_event_loop().run_in_executor(
                            None, mail_session.cleanup
                        )
                    except Exception:
                        pass

    tasks = [worker(i) for i in range(count)]
    await asyncio.gather(*tasks, return_exceptions=True)
    return results


async def retry_pending(
    cfg: Config,
    prog: Progress,
    proxy_url: str = "",
) -> list[dict]:
    """Recover sso_only accounts: mint tokens over HTTP from the saved SSO
    cookie (no browser), then move each success from pending into the backup."""
    from .device_flow import obtain_tokens
    from .export import load_pending

    pending = load_pending(cfg.output)
    if not pending:
        prog.log("no pending accounts to retry", "INFO", step="retry_pending")
        return []

    prog.total = len(pending)
    prog.log(f"retry-pending: {len(pending)} account(s)", "INFO", step="retry_pending")
    recovered: list[dict] = []

    for row in pending:
        email = str(row.get("email") or "")
        sso = str(row.get("sso") or "")
        if not email or not sso:
            prog.mark_fail(email or "(unknown)", "pending row missing email/sso")
            continue
        loop = asyncio.get_event_loop()
        tokens = await loop.run_in_executor(
            None, lambda: obtain_tokens(cfg, sso, proxy_url, prog, email)
        )
        if tokens and tokens.get("access_token"):
            export_row = _result_to_export({"email": email, "tokens": tokens, "sso_cookie": sso}, cfg)
            if export_row:
                n, path = write_backup([export_row], cfg.output, append=True)
                drop_pending_email(email, cfg.output)
                recovered.append(export_row)
                prog.log(f"recovered -> {path} (+{n})", "OK", email=email)
                prog.mark_ok(email, "recovered from pending")
                continue
        prog.mark_fail(email, "still no tokens (kept in pending)")

    prog.summary()
    return recovered
