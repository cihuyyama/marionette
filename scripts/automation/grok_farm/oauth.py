from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import re
import secrets
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from typing import Any
from urllib.parse import parse_qs, urlencode, urlparse

from .browser import recover_page_load_error
from .config import Config
from .login import (
    click_login_with_email,
    click_text_button,
    dismiss_cookie_banner,
    drive_email_password_login,
    wait_turnstile_passive,
)
from .progress import Progress


def generate_pkce_pair() -> tuple[str, str]:
    raw = secrets.token_bytes(96)
    verifier = base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")
    digest = hashlib.sha256(verifier.encode("ascii")).digest()
    challenge = base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")
    return verifier, challenge


def extract_code_from_url(url: str) -> str | None:
    try:
        parsed = urlparse(url)
    except Exception:
        return None
    host = (parsed.hostname or "").lower()
    if host not in ("127.0.0.1", "localhost"):
        return None
    if "/callback" not in (parsed.path or "") and "code=" not in url:
        return None
    params = parse_qs(parsed.query)
    vals = params.get("code")
    return vals[0] if vals else None


def build_authorize_url(cfg: Config, challenge: str, state: str, nonce: str) -> str:
    params = {
        "response_type": "code",
        "client_id": cfg.client_id,
        "redirect_uri": cfg.redirect_uri,
        "scope": cfg.scope,
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "state": state,
        "nonce": nonce,
        "plan": "generic",
        "referrer": "cli-proxy-api",
    }
    return f"{cfg.authorize_url}?{urlencode(params)}"


def exchange_code_for_tokens(code: str, verifier: str, cfg: Config) -> dict[str, Any]:
    form = urlencode(
        {
            "grant_type": "authorization_code",
            "client_id": cfg.client_id,
            "code": code,
            "redirect_uri": cfg.redirect_uri,
            "code_verifier": verifier,
        }
    ).encode("utf-8")
    req = urllib.request.Request(
        cfg.token_url,
        data=form,
        headers={
            "Content-Type": "application/x-www-form-urlencoded",
            "Accept": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        body = ""
        try:
            body = exc.read().decode("utf-8", errors="replace")[:300]
        except Exception:
            pass
        raise RuntimeError(f"token exchange HTTP {exc.code}: {body}") from exc

    access = data.get("access_token") or ""
    refresh = data.get("refresh_token") or ""
    if not access or not refresh:
        raise RuntimeError(f"token response missing tokens: {list(data.keys())}")

    expires_in = int(data.get("expires_in") or 21600)
    expires_at = datetime.now(timezone.utc).timestamp() + expires_in
    expires_at_iso = (
        datetime.fromtimestamp(expires_at, timezone.utc).isoformat().replace("+00:00", "Z")
    )

    email = ""
    id_token = data.get("id_token") or ""
    if id_token:
        try:
            payload_b64 = id_token.split(".")[1]
            payload_b64 += "=" * (-len(payload_b64) % 4)
            payload = json.loads(base64.urlsafe_b64decode(payload_b64).decode("utf-8"))
            email = payload.get("email") or ""
        except Exception:
            pass

    tokens: dict[str, Any] = {
        "access_token": access,
        "refresh_token": refresh,
        "expires_at": expires_at_iso,
        "expires_in": expires_in,
        "email": email,
        "client_id": cfg.client_id,
        "auth_mode": "oidc",
        "scope": data.get("scope") or cfg.scope,
    }
    if id_token:
        tokens["id_token"] = id_token
    return tokens


def _otp_form_present(page: Any) -> Any:
    return page.locator('input[name="code"], input[autocomplete="one-time-code"]')


async def fill_xai_otp_boxes(page: Any, otp_chars: str) -> bool:
    """Fill single or multi-slot OTP inputs. Thin helper."""
    chars = re.sub(r"[^A-Za-z0-9]", "", (otp_chars or "").upper())
    if len(chars) < 4:
        return False
    try:
        root = page.locator('input[name="code"], input[autocomplete="one-time-code"]').first
        if await root.count() > 0 and await root.is_visible():
            await root.click()
            await root.fill(chars)
            return True
    except Exception:
        pass
    # Multi-slot fallback
    try:
        slots = page.locator('input[maxlength="1"], input[inputmode="numeric"]')
        n = min(await slots.count(), len(chars))
        for i in range(n):
            await slots.nth(i).fill(chars[i])
        return n >= 4
    except Exception:
        return False


def read_otp_from_imap_sync(
    cfg: Config,
    target_email: str,
    timeout: int = 90,
) -> str | None:
    """
    Optional IMAP OTP reader. Raises RuntimeError with clear message if
    OTP is required but IMAP is not configured.
    """
    if not cfg.imap_configured:
        raise RuntimeError(
            "xAI requested OTP but IMAP is not configured. "
            "Set GROK_IMAP_HOST / GROK_IMAP_USER / GROK_IMAP_PASS in .env "
            "(or complete OTP manually in headed mode)."
        )
    # Thin stub: connect and scan recent messages for a 6–8 char code.
    # Full kit has richer xAI subject/body parsers — port more if needed.
    import email as email_lib
    import imaplib

    deadline = time.time() + timeout
    host = cfg.imap_host
    port = cfg.imap_port
    user = cfg.imap_user
    password = cfg.imap_pass
    target = (target_email or "").lower()

    while time.time() < deadline:
        try:
            M = imaplib.IMAP4_SSL(host, port)
            M.login(user, password)
            M.select("INBOX")
            typ, data = M.search(None, "UNSEEN")
            if typ != "OK":
                typ, data = M.search(None, "ALL")
            ids = (data[0] or b"").split()
            for mid in reversed(ids[-30:]):
                typ, msg_data = M.fetch(mid, "(RFC822)")
                if typ != "OK" or not msg_data or not msg_data[0]:
                    continue
                raw = msg_data[0][1]
                if not isinstance(raw, (bytes, bytearray)):
                    continue
                msg = email_lib.message_from_bytes(raw)
                to_hdr = str(msg.get("To") or "") + str(msg.get("Delivered-To") or "")
                subj = str(msg.get("Subject") or "")
                if target and target not in to_hdr.lower() and target.split("@")[0] not in to_hdr.lower():
                    # still allow codes from xAI without strict To match
                    if "x.ai" not in subj.lower() and "xai" not in subj.lower() and "grok" not in subj.lower():
                        continue
                body = ""
                if msg.is_multipart():
                    for part in msg.walk():
                        if part.get_content_type() == "text/plain":
                            try:
                                body += part.get_payload(decode=True).decode("utf-8", errors="replace")
                            except Exception:
                                pass
                else:
                    try:
                        body = msg.get_payload(decode=True).decode("utf-8", errors="replace")
                    except Exception:
                        body = str(msg.get_payload() or "")
                blob = f"{subj}\n{body}"
                m = re.search(r"\b([A-Z0-9]{6,8})\b", blob.upper())
                if m:
                    code = m.group(1)
                    # skip common English words
                    if code.lower() not in ("subject", "account", "message", "verify"):
                        try:
                            M.logout()
                        except Exception:
                            pass
                        return code
            try:
                M.logout()
            except Exception:
                pass
        except Exception:
            pass
        time.sleep(3.0)
    return None


async def handle_optional_otp(
    page: Any,
    email_addr: str,
    cfg: Config,
    prog: Progress,
    label: str,
) -> None:
    """If OTP fields appear, try IMAP (or raise clear error)."""
    if await _otp_form_present(page).count() == 0:
        return
    prog.step(label, "otp", "OTP field detected")
    if not cfg.imap_configured and not cfg.headless:
        prog.log(
            "OTP shown — IMAP not set; waiting 60s for manual entry (headed)",
            "WAIT",
            email=label,
        )
        deadline = time.monotonic() + 60.0
        while time.monotonic() < deadline:
            # user may type manually
            try:
                val = await page.evaluate(
                    """() => {
                        const el = document.querySelector(
                            'input[name="code"], input[autocomplete="one-time-code"]'
                        );
                        return el ? (el.value || '') : '';
                    }"""
                )
                if val and len(re.sub(r"[^A-Za-z0-9]", "", str(val))) >= 4:
                    await click_text_button(page, ["Confirm", "Verify", "Continue", "Submit"])
                    await asyncio.sleep(1.5)
                    return
            except Exception:
                pass
            await asyncio.sleep(1.0)
        raise RuntimeError(
            "OTP required but not completed (IMAP not configured; manual wait timed out)"
        )

    loop = asyncio.get_event_loop()
    otp = await loop.run_in_executor(
        None, lambda: read_otp_from_imap_sync(cfg, email_addr, 90)
    )
    if not otp:
        raise RuntimeError("OTP required but IMAP did not return a code in time")
    chars = re.sub(r"[^A-Z0-9]", "", otp.upper())
    ok = await fill_xai_otp_boxes(page, chars)
    if not ok:
        raise RuntimeError("failed to fill OTP boxes")
    await click_text_button(page, ["Confirm", "Verify", "Continue", "Submit"])
    await asyncio.sleep(1.5)
    prog.log("OTP submitted", "OK", email=label)


async def obtain_oidc_tokens(
    page: Any,
    email_addr: str,
    password: str,
    cfg: Config,
    prog: Progress,
    label: str,
) -> dict[str, Any]:
    """
    PKCE authorize → email sign-in / consent → capture code via route to
    127.0.0.1:56121 → exchange_code_for_tokens.
    """
    prog.step(label, "oauth", "Grok CLI OAuth PKCE")
    verifier, challenge = generate_pkce_pair()
    state = secrets.token_urlsafe(24)
    nonce = secrets.token_hex(16)
    auth_url = build_authorize_url(cfg, challenge, state, nonce)
    auth_code: dict[str, str | None] = {"code": None}

    async def _handle_route(route: Any) -> None:
        req_url = route.request.url
        if (
            req_url.startswith("http://127.0.0.1:56121/")
            or req_url.startswith("http://localhost:56121/")
            or (
                "/callback" in req_url
                and ("127.0.0.1" in req_url or "localhost" in req_url)
            )
        ):
            code = extract_code_from_url(req_url)
            if code:
                auth_code["code"] = code
                prog.log("OAuth code captured via route", "OK", email=label, step="oauth")
            try:
                await route.abort()
            except Exception:
                pass
            return
        try:
            rtype = route.request.resource_type
        except Exception:
            rtype = ""
        if rtype in ("image", "font", "media"):
            try:
                await route.abort()
            except Exception:
                pass
            return
        try:
            await route.continue_()
        except Exception:
            pass

    await page.route("**/*", _handle_route)
    try:
        try:
            await page.goto(auth_url, wait_until="domcontentloaded", timeout=45_000)
        except Exception:
            await page.goto(auth_url, wait_until="commit", timeout=45_000)

        deadline = time.monotonic() + float(cfg.oauth_timeout)
        while time.monotonic() < deadline and not auth_code.get("code"):
            try:
                cur = page.url or ""
                code = extract_code_from_url(cur)
                if code:
                    auth_code["code"] = code
                    break
            except Exception:
                cur = ""

            await recover_page_load_error(page)
            await wait_turnstile_passive(page, max_wait=6.0)

            if "/oauth2/consent" in cur:
                await dismiss_cookie_banner(page)
                clicked = await click_text_button(
                    page,
                    ["Allow", "Authorize", "Approve", "Accept", "Continue", "Confirm", "Grant"],
                    exclude=["Google", "Deny", "Cancel", "Go back", "Sign in", "Log in", "Sign up"],
                )
                if clicked:
                    prog.log(f"consent clicked: {clicked!r}", "OK", email=label, step="consent")
                    await asyncio.sleep(2.0)
            elif "accounts.x.ai" in cur or "auth.x.ai" in cur:
                await dismiss_cookie_banner(page)
                if await page.locator('input[type="email"], input[type="password"]').count() == 0:
                    await click_login_with_email(page)
                    await asyncio.sleep(0.8)
                has_form = (
                    await page.locator('input[type="email"], input[type="password"]').count() > 0
                )
                has_email_btn = (
                    await page.locator(
                        "text=/Login with email|Log in with email|Sign in with email/i"
                    ).count()
                    > 0
                )
                if has_form or has_email_btn:
                    await drive_email_password_login(page, email_addr, password, prog, label)
                if await _otp_form_present(page).count() > 0:
                    await handle_optional_otp(page, email_addr, cfg, prog, label)

            await asyncio.sleep(1.0)

        if not auth_code.get("code"):
            try:
                for p in page.context.pages:
                    c = extract_code_from_url(p.url or "")
                    if c:
                        auth_code["code"] = c
                        break
            except Exception:
                pass

        code = auth_code.get("code")
        if not code:
            try:
                cur = (page.url or "")[:160]
                hint = await page.evaluate(
                    """() => {
                        const t = (document.body && document.body.innerText || '').slice(0, 200);
                        return t.replace(/\\s+/g, ' ').trim();
                    }"""
                )
            except Exception:
                cur, hint = "", ""
            raise RuntimeError(
                f"OAuth code not captured (timeout). url={cur!r} page={hint[:120]!r}. "
                "Common causes: Login-with-email not clicked, Turnstile stuck, "
                "page load error, Access denied, or OTP without IMAP."
            )

        prog.step(label, "token_exchange", "exchanging code for tokens")
        tokens = exchange_code_for_tokens(code, verifier, cfg)
        if not tokens.get("email"):
            tokens["email"] = email_addr
        return tokens
    finally:
        try:
            await page.unroute("**/*")
        except Exception:
            pass
