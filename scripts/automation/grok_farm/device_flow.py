from __future__ import annotations

import base64
import json
import time
from datetime import datetime, timezone
from typing import Any

from .config import Config

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


def principal_id_from_sso(sso_cookie: str) -> str:
    pl = decode_jwt_payload(sso_cookie)
    for key in ("principal_id", "sub", "user_id", "uid", "session_id"):
        val = pl.get(key)
        if val:
            return str(val)
    return ""


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


def verify(session: Any, user_code: str, proxy_url: str = "") -> bool:
    kwargs: dict[str, Any] = {"impersonate": "chrome", "timeout": 30, "allow_redirects": True, **_proxy_kwargs(proxy_url)}
    for vurl in ("https://accounts.x.ai/oauth2/device/verify", "https://auth.x.ai/oauth2/device/verify"):
        try:
            r = session.post(vurl, data={"user_code": user_code}, headers={"Content-Type": "application/x-www-form-urlencoded"}, **kwargs)
            if r.status_code < 400:
                return True
        except Exception:
            continue
    return False


def approve(session: Any, user_code: str, principal_id: str, proxy_url: str = "") -> bool:
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
            if r.status_code < 400:
                return True
        except Exception:
            continue
    return False


def poll_token(cfg: Config, device_code: str, interval: int = 1, timeout: float = 45, proxy_url: str = "") -> dict | None:
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
    return None


def obtain_tokens(cfg: Config, sso_cookie: str, proxy_url: str = "", prog: Any = None, email: str = "") -> dict | None:
    from curl_cffi import requests as cffi

    principal_id = principal_id_from_sso(sso_cookie)
    if prog:
        prog.log(f"device flow: principal_id={'...' + principal_id[-8:] if principal_id else '(empty)'}", "DBG", email=email, step="device_flow")

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
            prog.log(f"device flow: user_code={user_code}", "DBG", email=email, step="device_flow")

        try:
            vuri = dc.get("verification_uri_complete", "")
            if vuri:
                session.get(vuri, timeout=20, **pkw)
        except Exception:
            pass

        if not verify(session, user_code, proxy_url):
            if flow_attempt < 2:
                time.sleep(3)
                continue
            return None

        if not approve(session, user_code, principal_id, proxy_url):
            if flow_attempt < 2:
                time.sleep(3)
                continue
            return None

        token_data = poll_token(cfg, device_code, interval=dc.get("interval", 1), timeout=45, proxy_url=proxy_url)
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
