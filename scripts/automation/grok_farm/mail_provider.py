"""Temp-mail provider for signup OTP reception.

Backend: self-hosted dreamhunter2333/cloudflare_temp_email Worker. The worker
is catch-all on the bound domain, but we still pre-create the address via the
admin API to obtain a per-address JWT for scoped polling.

API surface (worker commit d04c1a8):
  POST   /admin/new_address            x-admin-auth -> {jwt, address, address_id}
  GET    /api/parsed_mails?limit&offset Bearer <address-jwt> -> {results, count}
  DELETE /admin/delete_address/:id     x-admin-auth
Optional site-wide password mode adds x-custom-auth to every non-/open_api call.
"""
from __future__ import annotations

import json
import random
import secrets
import string
import time
import urllib.error
import urllib.request
from dataclasses import dataclass

from .config import Config

_HTTP_TIMEOUT = 20
# Cloudflare WAF rejects the default Python-urllib UA on custom domains (err 1010).
_USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36"
)


def _http_json(
    url: str,
    method: str = "GET",
    body: dict | None = None,
    headers: dict | None = None,
) -> dict:
    req = urllib.request.Request(url, method=method)
    req.add_header("User-Agent", _USER_AGENT)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, data=data, timeout=_HTTP_TIMEOUT) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            return json.loads(raw) if raw.strip() else {}
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")[:200]
        raise RuntimeError(f"temp-mail API {exc.code} on {url}: {detail}") from exc


class CFTempMailClient:
    def __init__(self, base_url: str, admin_password: str, domain: str, site_password: str = ""):
        self.base_url = base_url.rstrip("/")
        self.admin_password = admin_password
        self.domain = domain
        self.site_password = site_password

    def _common_headers(self) -> dict:
        headers = {"x-admin-auth": self.admin_password}
        if self.site_password:
            headers["x-custom-auth"] = self.site_password
        return headers

    def create_address(self, name: str | None = None) -> tuple[str, str, int | None]:
        if not name:
            chars = string.ascii_lowercase + string.digits
            name = "".join(secrets.choice(chars) for _ in range(random.randint(14, 18)))
        payload = {"name": name, "domain": self.domain, "enablePrefix": False}
        res = _http_json(
            f"{self.base_url}/admin/new_address",
            method="POST",
            body=payload,
            headers=self._common_headers(),
        )
        address = str(res.get("address") or "")
        jwt = str(res.get("jwt") or "")
        if not address or not jwt:
            raise RuntimeError(f"temp-mail create_address bad response: {str(res)[:200]}")
        address_id = res.get("address_id")
        return address, jwt, address_id if isinstance(address_id, int) else None

    def fetch_parsed_mails(self, jwt: str, limit: int = 20) -> list[dict]:
        headers = {"Authorization": f"Bearer {jwt}"}
        if self.site_password:
            headers["x-custom-auth"] = self.site_password
        res = _http_json(
            f"{self.base_url}/api/parsed_mails?limit={limit}&offset=0",
            headers=headers,
        )
        results = res.get("results")
        return results if isinstance(results, list) else []

    def delete_address(self, address_id: int) -> None:
        _http_json(
            f"{self.base_url}/admin/delete_address/{address_id}",
            method="DELETE",
            headers=self._common_headers(),
        )


@dataclass
class TempMailSession:
    """One mailbox for one signup attempt. poll_otp/cleanup are blocking —
    run inside an executor from async code (same pattern as read_otp_imap)."""

    email: str
    jwt: str
    address_id: int | None
    client: CFTempMailClient
    extract_otp: object  # callable(subject, body) -> str | None

    def poll_otp(self, timeout: int = 180, interval: float = 3.0) -> str | None:
        deadline = time.time() + timeout
        seen_ids: set = set()
        while time.time() < deadline:
            try:
                mails = self.client.fetch_parsed_mails(self.jwt)
            except Exception:
                mails = []
            for mail in mails:
                mid = mail.get("id")
                if mid in seen_ids:
                    continue
                seen_ids.add(mid)
                subject = str(mail.get("subject") or "")
                body = " ".join(
                    str(mail.get(k) or "") for k in ("text", "html")
                )
                code = self.extract_otp(subject, body)  # type: ignore[misc]
                if code:
                    return code
            time.sleep(interval)
        return None

    def cleanup(self) -> None:
        if self.address_id is None:
            return
        try:
            self.client.delete_address(self.address_id)
        except Exception:
            pass


def cf_mail_configured(cfg: Config) -> bool:
    return bool(cfg.cf_mail_base_url and cfg.cf_mail_admin_password and cfg.cf_mail_domain)


def create_cf_client(cfg: Config) -> CFTempMailClient:
    return CFTempMailClient(
        base_url=cfg.cf_mail_base_url,
        admin_password=cfg.cf_mail_admin_password,
        domain=cfg.cf_mail_domain,
        site_password=cfg.cf_mail_site_password,
    )
