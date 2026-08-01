from __future__ import annotations

import asyncio
import imaplib
import json
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
from .browser import _normalize_proxy_url, _proxy_dict
from .config import Config
from .device_flow import obtain_tokens
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
    m = _XAI_SUBJ_CODE_RE.search(subject or "")
    if m and _is_plausible_otp(m.group(1)):
        return m.group(1).upper()
    for m in _XAI_CODE_RE.finditer(subject or ""):
        if _is_plausible_otp(m.group(1)):
            return m.group(1).upper()
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


async def _fill_otp(page: Any, code: str, prog: Progress) -> bool:
    code_clean = re.sub(r"[^A-Za-z0-9]", "", code).upper()
    for _ in range(5):
        try:
            otp_input = page.locator('input[name="code"], input[autocomplete="one-time-code"]')
            if await otp_input.count() > 0:
                await otp_input.first.fill(code_clean)
                await asyncio.sleep(0.3)
                btn = page.get_by_role("button", name=re.compile(r"confirm|verify|continue|submit", re.I))
                if await btn.count() > 0:
                    await btn.first.click(force=True)
                    return True
            slots = page.locator('input[maxlength="1"]')
            if await slots.count() >= 6:
                await slots.first.click()
                await page.keyboard.type(code_clean, delay=50)
                await asyncio.sleep(0.5)
                btn = page.get_by_role("button", name=re.compile(r"confirm|verify|continue|submit", re.I))
                if await btn.count() > 0:
                    await btn.first.click(force=True)
                    return True
        except Exception:
            pass
        await asyncio.sleep(1)
    return False


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


async def _handle_turnstile_checkbox(page: Any, prog: Progress, max_wait: float = 25) -> bool:
    deadline = time.monotonic() + max_wait
    while time.monotonic() < deadline:
        try:
            iframe = page.frame_locator('iframe[src*="challenges.cloudflare.com"]')
            checkbox = iframe.locator('input[type="checkbox"], .cbx, #cf-turnstile-checkbox')
            if await checkbox.count() > 0:
                await checkbox.first.click(force=True, timeout=3000)
                await asyncio.sleep(3)
                token = await page.evaluate(
                    "() => { const el = document.querySelector('[name=\"cf-turnstile-response\"]'); return el ? el.value.length : 0; }"
                )
                if token and int(token) > 20:
                    return True
        except Exception:
            pass
        await asyncio.sleep(1.5)
    return False


async def _extract_sso_cookie(page: Any) -> str | None:
    try:
        cookies = await page.context.cookies()
        for c in cookies:
            if c.get("name") == "sso":
                return c["value"]
    except Exception:
        pass
    return None


async def register_one(
    page: Any,
    email: str,
    password: str,
    cfg: Config,
    prog: Progress,
    proxy_url: str = "",
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
        return None

    prog.step(email, "wait_otp", "Waiting for OTP via IMAP")
    otp = await asyncio.get_event_loop().run_in_executor(
        None, read_otp_imap, cfg, email, 180
    )
    if not otp:
        prog.log("OTP timeout (180s)", "ERR", email=email, step="wait_otp")
        return None
    prog.log(f"OTP received: {otp}", "DBG", email=email, step="wait_otp")

    prog.step(email, "confirm_otp", "Confirming OTP")
    if not await _fill_otp(page, otp, prog):
        prog.log("OTP fill failed", "ERR", email=email, step="confirm_otp")
        return None
    await asyncio.sleep(2)

    prog.step(email, "profile", "Filling profile + password")
    await _fill_profile_and_password(page, password, prog)

    prog.step(email, "turnstile", "Solving Turnstile")
    await _handle_turnstile_checkbox(page, prog, max_wait=25)

    submit_btn = page.get_by_role("button", name=re.compile(r"complete|sign up|create|submit", re.I))
    try:
        if await submit_btn.count() > 0:
            await submit_btn.first.click(force=True, timeout=5000)
    except Exception:
        pass
    await asyncio.sleep(3)

    prog.step(email, "sso_extract", "Extracting SSO cookie")
    sso_cookie = await _extract_sso_cookie(page)
    if not sso_cookie:
        try:
            await page.goto("https://grok.com", wait_until="domcontentloaded", timeout=30000)
            await asyncio.sleep(5)
            sso_cookie = await _extract_sso_cookie(page)
        except Exception:
            pass
    if not sso_cookie:
        prog.log("SSO cookie not found", "ERR", email=email, step="sso_extract")
        return None
    prog.log(f"SSO cookie: {sso_cookie[:12]}...", "DBG", email=email, step="sso_extract")

    prog.step(email, "device_flow", "Running OAuth Device Flow")
    tokens = await asyncio.get_event_loop().run_in_executor(
        None, obtain_tokens, cfg, sso_cookie, proxy_url, prog, email
    )
    if not tokens:
        prog.log("Device Flow failed", "ERR", email=email, step="device_flow")
        return None

    return {
        "email": email,
        "password": password,
        "sso_cookie": sso_cookie,
        "tokens": tokens,
        "created_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
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

    if not email_source:
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
    proxy_idx = 0

    async def worker(idx: int) -> None:
        nonlocal proxy_idx
        async with sem:
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
                    result = await register_one(page, email, password, cfg, prog, proxy_url)
                    if result:
                        results.append(result)
                        prog.mark_ok(email, "registered + tokens obtained")
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

    tasks = [worker(i) for i in range(count)]
    await asyncio.gather(*tasks, return_exceptions=True)
    return results
