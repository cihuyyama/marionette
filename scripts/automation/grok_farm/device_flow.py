from __future__ import annotations

import asyncio
import base64
import json
import time
from datetime import datetime, timezone
from typing import Any

from .activate import activate_grok_if_needed
from .browser import recover_page_load_error
from .config import Config
from .login import click_login_with_email, click_text_button, dismiss_cookie_banner, drive_email_password_login
from .oauth import _consent_force_allow, _consent_page_denied, handle_optional_otp, obtain_oidc_tokens

DEVICE_REFERRER = "grok-build"


def decode_jwt_payload(token: str) -> dict:
    try:
        parts = token.split(".")
        if len(parts) < 2:
            return {}
        payload_b64 = parts[1]
        payload_b64 += "=" * (-len(payload_b64) % 4)
        return json.loads(base64.urlsafe_b64decode(payload_b64).decode("utf-8"))
    except Exception:
        return {}


def principal_id_from_sso(sso_cookie: str) -> tuple[str, str]:
    pl = decode_jwt_payload(sso_cookie)
    for key in ("principal_id", "sub", "user_id", "uid", "session_id"):
        val = pl.get(key)
        if val:
            return str(val), key
    return "", ""


def _set_sso_cookies(session: Any, sso_cookie: str) -> None:
    for domain in (".x.ai", "accounts.x.ai", "auth.x.ai", ".accounts.x.ai"):
        session.cookies.set("sso", sso_cookie, domain=domain, path="/")


def _proxy_kwargs(proxy_url: str) -> dict:
    if not proxy_url:
        return {}
    return {"proxies": {"http": proxy_url, "https": proxy_url}}


def request_device_code(cfg: Config, proxy_url: str = "") -> dict | None:
    from curl_cffi import requests as cffi

    form = {"client_id": cfg.client_id, "scope": cfg.scope}
    kwargs: dict[str, Any] = {"impersonate": "chrome", "timeout": 30, **_proxy_kwargs(proxy_url)}
    for attempt in range(3):
        try:
            r = cffi.post(
                "https://auth.x.ai/oauth2/device/code",
                data=form,
                headers={"Content-Type": "application/x-www-form-urlencoded"},
                **kwargs,
            )
            if r.status_code < 400:
                data = r.json()
                if isinstance(data, dict) and data.get("device_code"):
                    return data
            if r.status_code == 429 and attempt < 2:
                time.sleep(3 * (attempt + 1))
                continue
            return None
        except Exception:
            if attempt < 2:
                time.sleep(2)
    return None


def verify(session: Any, user_code: str, proxy_url: str = "", prog: Any = None, email: str = "") -> bool:
    kwargs: dict[str, Any] = {"impersonate": "chrome", "timeout": 30, "allow_redirects": True, **_proxy_kwargs(proxy_url)}
    for vurl in ("https://accounts.x.ai/oauth2/device/verify", "https://auth.x.ai/oauth2/device/verify"):
        try:
            r = session.post(vurl, data={"user_code": user_code}, headers={"Content-Type": "application/x-www-form-urlencoded"}, **kwargs)
            if prog:
                prog.log(f"device verify {vurl.split('//')[-1].split('/')[0]} -> {r.status_code}", "DBG", email=email, step="device_flow")
            if r.status_code < 400:
                return True
        except Exception as e:
            if prog:
                prog.log(f"device verify err: {e}", "DBG", email=email, step="device_flow")
            continue
    return False


def approve(session: Any, user_code: str, principal_id: str, proxy_url: str = "", prog: Any = None, email: str = "") -> bool:
    kwargs: dict[str, Any] = {"impersonate": "chrome", "timeout": 30, "allow_redirects": True, **_proxy_kwargs(proxy_url)}
    payload = {
        "user_code": user_code,
        "action": "allow",
        "principal_type": "User",
        "principal_id": principal_id,
        "referrer": DEVICE_REFERRER,
    }
    for aurl in ("https://accounts.x.ai/oauth2/device/approve", "https://auth.x.ai/oauth2/device/approve"):
        try:
            r = session.post(aurl, data=payload, headers={"Content-Type": "application/x-www-form-urlencoded"}, **kwargs)
            if prog:
                body = ""
                try:
                    body = (r.text or "")[:160]
                except Exception:
                    pass
                prog.log(f"device approve {aurl.split('//')[-1].split('/')[0]} -> {r.status_code} {body}", "DBG", email=email, step="device_flow")
            if r.status_code < 400:
                return True
        except Exception as e:
            if prog:
                prog.log(f"device approve err: {e}", "DBG", email=email, step="device_flow")
            continue
    return False


def poll_token(cfg: Config, device_code: str, interval: int = 1, timeout: float = 45, proxy_url: str = "", prog: Any = None, email: str = "") -> dict | None:
    from curl_cffi import requests as cffi

    form = {
        "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        "client_id": cfg.client_id,
        "device_code": device_code,
    }
    kwargs: dict[str, Any] = {"impersonate": "chrome", "timeout": 15, **_proxy_kwargs(proxy_url)}
    deadline = time.time() + timeout
    poll_interval = max(1.0, float(interval))
    first = True
    last_error = ""
    while time.time() < deadline:
        if not first:
            time.sleep(poll_interval)
        first = False
        try:
            r = cffi.post(
                "https://auth.x.ai/oauth2/token",
                data=form,
                headers={"Content-Type": "application/x-www-form-urlencoded"},
                **kwargs,
            )
            if r.status_code < 400:
                data = r.json()
                if isinstance(data, dict) and data.get("access_token"):
                    return data
            try:
                err = r.json() if r.content else {}
            except Exception:
                err = {}
            error = str((err or {}).get("error") or "")
            if error != last_error:
                last_error = error
                if prog:
                    prog.log(f"device poll {r.status_code} error={error or '(none)'}", "DBG", email=email, step="device_flow")
            if error == "authorization_pending":
                continue
            if error == "slow_down":
                poll_interval = min(10.0, poll_interval + 1.0)
                continue
            if error == "invalid_grant":
                return None
            return None
        except Exception:
            if time.time() >= deadline:
                return None
    if prog:
        prog.log(f"device poll timeout (last error={last_error or '(none)'})", "DBG", email=email, step="device_flow")
    return None


def _validate_device_tokens(tokens: Any) -> bool:
    """Final success gate: both access_token and refresh_token must be present
    and non-empty. HTTP poll success is the only authority."""
    if not isinstance(tokens, dict):
        return False
    return bool(tokens.get("access_token")) and bool(tokens.get("refresh_token"))


def _safe_url_path(url: str) -> str:
    """Return hostname/path only — never query strings or tokens."""
    try:
        from urllib.parse import urlparse

        p = urlparse(url or "")
        host = p.hostname or ""
        path = p.path or ""
        return f"{host}{path}" if host else path
    except Exception:
        return ""


async def _device_page_state(page: Any, *, token_ready: bool = False) -> str:
    """Classify the current device-flow page.

    Priority: token -> done -> error -> login -> consent -> unknown.
    """
    if token_ready:
        return "token"

    try:
        cur = (page.url or "").lower()
    except Exception:
        cur = ""

    if "device/done" in cur or "device/approved" in cur:
        return "done"

    try:
        body = await page.evaluate(
            """() => ((document.body && document.body.innerText) || '')
                .replace(/\\s+/g, ' ').trim().slice(0, 600)"""
        )
    except Exception:
        body = ""
    low = (body or "").lower()
    if "failed to generate authentication code" in low or (
        "access denied" in low and "authoriz" in low
    ):
        return "error"

    try:
        has_password = await page.locator('input[type="password"]').count() > 0
    except Exception:
        has_password = False
    if has_password:
        return "login"

    try:
        has_email_input = await page.locator('input[type="email"], input[name="email"]').count() > 0
    except Exception:
        has_email_input = False
    if has_email_input:
        return "login"

    try:
        has_email_choice = await page.locator(
            "text=/Continue with email|Sign in with email|Login with email|Log in with email/i"
        ).count() > 0
    except Exception:
        has_email_choice = False
    if has_email_choice:
        return "login"

    if "/consent" in cur:
        return "consent"
    if "authorize" in low and "allow" in low:
        return "consent"

    if "/oauth2/device" in cur and "device/done" not in cur and "device/approved" not in cur:
        return "device_code"
    try:
        has_user_code_input = await page.locator('input[name="user_code"]').count() > 0
    except Exception:
        has_user_code_input = False
    if has_user_code_input:
        return "device_code"

    return "unknown"


async def _drive_device_confirmation(
    page: Any,
    state: str,
    *,
    email: str,
    password: str,
    prog: Any = None,
    label: str = "",
    cfg: Config | None = None,
    user_code: str = "",
) -> bool:
    """React to a classified device-page state. Returns True if the state was
    handled (approval dispatched / done reached), False if no progress."""
    if state == "token":
        return True

    if state == "done":
        return True

    if state == "error":
        reason = "device confirmation denied by xAI"
        if prog:
            prog.log(reason, "ERR", email=label, step="device_flow")
        raise RuntimeError(reason)

    if state == "login":
        if prog:
            prog.log("device page: login form detected — driving credentials", "DBG", email=label, step="device_flow")
        try:
            has_email_choice = await page.locator(
                "text=/Continue with email|Sign in with email|Login with email|Log in with email/i"
            ).count() > 0
            has_password = await page.locator('input[type="password"]').count() > 0
        except Exception:
            has_email_choice = False
            has_password = False
        if has_email_choice and not has_password:
            await click_login_with_email(page)
            await asyncio.sleep(1.0)
        login_ok = await drive_email_password_login(page, email, password, prog, label)
        if login_ok and cfg is not None:
            await handle_optional_otp(page, email, cfg, prog, label)
        return login_ok

    if state == "consent":
        # Only explicit approval verbs — Continue must NOT precede or masquerade.
        clicked = await click_text_button(
            page,
            ["Allow", "Authorize", "Approve"],
            exclude=["Deny", "Cancel", "Go back", "Sign up", "Sign in", "Log in", "Google", "Apple"],
        )
        if clicked:
            if prog:
                prog.log(f"device consent clicked: {clicked!r}", "OK", email=label, step="device_flow")
            return True
        if await _consent_force_allow(page):
            if prog:
                prog.log("device consent force-allow (form action=allow)", "OK", email=label, step="device_flow")
            return True
        return False

    if state == "device_code":
        try:
            code_input = page.locator('input[name="user_code"]')
            if await code_input.count() > 0 and await code_input.first.is_visible():
                current_val = await code_input.first.input_value()
                if not current_val and user_code:
                    await code_input.first.fill(user_code)
                    await asyncio.sleep(0.3)
        except Exception:
            pass
        clicked = await click_text_button(
            page,
            ["Continue", "Submit", "Verify", "Confirm"],
            exclude=["Deny", "Cancel", "Go back", "Sign up", "Sign in", "Log in", "Google", "Apple"],
        )
        if clicked and prog:
            prog.log(f"device code submitted ({clicked!r})", "OK", email=label, step="device_flow")
        return bool(clicked)

    await dismiss_cookie_banner(page)
    await recover_page_load_error(page)
    await asyncio.sleep(1.5)
    return False


async def _device_screenshot(page: Any, cfg: Config, label: str, tag: str, prog: Any = None) -> None:
    """Sanitized diagnostic screenshot before failure return."""
    try:
        cfg.screenshot_dir.mkdir(parents=True, exist_ok=True)
        safe = (label or "unknown").replace("@", "_at_").replace(".", "_")
        safe = "".join(c if c.isalnum() or c in ("_", "-") else "_" for c in safe)
        path = cfg.screenshot_dir / f"{safe}_{tag}.png"
        await page.screenshot(path=str(path), full_page=True)
        if prog:
            prog.log(f"device screenshot -> {path.name}", "DBG", email=label, step="device_flow")
    except Exception:
        pass


async def obtain_tokens_via_browser(
    page: Any,
    cfg: Config,
    prog: Any = None,
    email: str = "",
    password: str = "",
    proxy_url: str = "",
    approve_timeout: float = 120.0,
) -> dict | None:
    # xAI rejects pure-HTTP device approve for fresh accounts (invalid_grant /
    # Access denied), but accepts approval driven through the already-signed-in
    # browser session. Token endpoint is polled off-thread while the page
    # reactively handles login/consent/done states; HTTP poll success is the
    # only final success (Aaron reference c72e3b7).

    dc = request_device_code(cfg, proxy_url)
    if not dc or not dc.get("device_code"):
        if prog:
            prog.log("device code request failed", "ERR", email=email, step="device_flow")
        return None
    device_code = dc.get("device_code", "")
    user_code = dc.get("user_code", "")
    vuri = dc.get("verification_uri_complete") or dc.get("verification_uri") or ""
    if prog:
        prog.log("device flow (browser): code requested", "DBG", email=email, step="device_flow")

    token_box: dict[str, Any] = {}

    def _poll() -> None:
        token_box["tokens"] = poll_token(
            cfg,
            device_code,
            interval=int(dc.get("interval") or 5),
            timeout=approve_timeout,
            proxy_url=proxy_url,
            prog=prog,
            email=email,
        )

    poll_task = asyncio.get_event_loop().run_in_executor(None, _poll)

    try:
        await page.goto(vuri, wait_until="domcontentloaded", timeout=45000)
    except Exception:
        try:
            await page.goto(vuri, wait_until="commit", timeout=45000)
        except Exception:
            pass

    deadline = time.time() + approve_timeout
    browser_done = False
    while time.time() < deadline and not _validate_device_tokens(token_box.get("tokens")):
        state = await _device_page_state(
            page, token_ready=_validate_device_tokens(token_box.get("tokens"))
        )
        if state == "done":
            browser_done = True
            if prog:
                prog.log("device page reached done — awaiting token poll", "DBG", email=email, step="device_flow")
            break
        try:
            await _drive_device_confirmation(
                page, state, email=email, password=password, prog=prog, label=email, cfg=cfg,
                user_code=user_code,
            )
        except RuntimeError:
            break
        await asyncio.sleep(1.0)

    try:
        await poll_task
    except Exception:
        pass

    token_data = token_box.get("tokens")
    if not _validate_device_tokens(token_data):
        url_path = _safe_url_path(page.url if hasattr(page, "url") else "")
        if prog:
            prog.log(
                f"device flow (browser) no token (done={browser_done}, url={url_path})",
                "ERR",
                email=email,
                step="device_flow",
            )
        await _device_screenshot(page, cfg, email, "device_no_token", prog)
        return None

    access = token_data.get("access_token", "")
    refresh = token_data.get("refresh_token", "")
    expires_in = int(token_data.get("expires_in") or 21600)
    expires_at = datetime.now(timezone.utc).timestamp() + expires_in
    expires_at_iso = datetime.fromtimestamp(expires_at, timezone.utc).isoformat().replace("+00:00", "Z")
    id_token = token_data.get("id_token") or ""
    email_claim = decode_jwt_payload(id_token).get("email", "") if id_token else ""
    if prog:
        prog.log(f"device flow (browser) OK: expires_in={expires_in}s", "OK", email=email, step="device_flow")
    return {
        "access_token": access,
        "refresh_token": refresh,
        "expires_at": expires_at_iso,
        "expires_in": expires_in,
        "email": email_claim or email,
        "client_id": cfg.client_id,
        "auth_mode": "oidc",
        "scope": token_data.get("scope") or cfg.scope,
        "id_token": id_token,
    }


async def obtain_tokens_with_retry(
    page: Any,
    email: str,
    password: str,
    cfg: Config,
    prog: Any,
    proxy_url: str = "",
) -> dict | None:
    """Unified token acquisition: browser device flow first, PKCE fallback.

    Shared by register and relogin so both mint tokens through the same
    path. The browser device flow is preferred for accounts the browser is
    already signed into (it avoids a second Turnstile round on the PKCE
    authorize page); PKCE remains the fallback per attempt.
    """
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
            prog.log(
                f"oauth retry {attempt}/{attempts - 1} (backoff {backoff:.0f}s)",
                "WAIT",
                email=email,
                step="oauth",
            )
            await asyncio.sleep(backoff)
            await activate_grok_if_needed(page, cfg, prog, email, attempts=2)
    return None


def obtain_tokens(cfg: Config, sso_cookie: str, proxy_url: str = "", prog: Any = None, email: str = "") -> dict | None:
    from curl_cffi import requests as cffi

    principal_id, pid_key = principal_id_from_sso(sso_cookie)
    if prog:
        prog.log(
            f"device flow: principal_id={'...' + principal_id[-8:] if principal_id else '(empty)'} (from {pid_key or 'none'})",
            "DBG",
            email=email,
            step="device_flow",
        )

    session = cffi.Session(impersonate="chrome")
    _set_sso_cookies(session, sso_cookie)
    pkw = _proxy_kwargs(proxy_url)

    try:
        r = session.get("https://accounts.x.ai/", timeout=20, **pkw)
        if "sign-in" in r.url or "sign-up" in r.url:
            if prog:
                prog.log("device flow: SSO invalid", "WARN", email=email, step="device_flow")
            return None
    except Exception as e:
        if prog:
            prog.log(f"device flow: SSO check error: {e}", "WARN", email=email, step="device_flow")
        return None

    for flow_attempt in range(3):
        dc = request_device_code(cfg, proxy_url)
        if not dc:
            if flow_attempt < 2:
                time.sleep(3 * (flow_attempt + 1))
                continue
            return None
        user_code = dc.get("user_code", "")
        device_code = dc.get("device_code", "")
        if prog:
            prog.log("device flow: code requested", "DBG", email=email, step="device_flow")

        try:
            vuri = dc.get("verification_uri_complete", "")
            if vuri:
                session.get(vuri, timeout=20, **pkw)
        except Exception:
            pass

        if not verify(session, user_code, proxy_url, prog=prog, email=email):
            if flow_attempt < 2:
                time.sleep(3)
                continue
            return None

        if not approve(session, user_code, principal_id, proxy_url, prog=prog, email=email):
            if flow_attempt < 2:
                time.sleep(3)
                continue
            return None

        token_data = poll_token(cfg, device_code, interval=dc.get("interval", 1), timeout=45, proxy_url=proxy_url, prog=prog, email=email)
        if not token_data:
            if flow_attempt < 2:
                time.sleep(3)
                continue
            return None

        access = token_data.get("access_token", "")
        refresh = token_data.get("refresh_token", "")
        if not access:
            return None
        expires_in = int(token_data.get("expires_in") or 21600)
        expires_at = datetime.now(timezone.utc).timestamp() + expires_in
        expires_at_iso = datetime.fromtimestamp(expires_at, timezone.utc).isoformat().replace("+00:00", "Z")
        id_token = token_data.get("id_token") or ""
        email_claim = ""
        if id_token:
            email_claim = decode_jwt_payload(id_token).get("email", "")
        if prog:
            prog.log(f"device flow OK: expires_in={expires_in}s", "OK", email=email, step="device_flow")
        return {
            "access_token": access,
            "refresh_token": refresh,
            "expires_at": expires_at_iso,
            "expires_in": expires_in,
            "email": email_claim,
            "client_id": cfg.client_id,
            "auth_mode": "oidc",
            "scope": token_data.get("scope") or cfg.scope,
            "id_token": id_token,
        }
    return None
