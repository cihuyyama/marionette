from __future__ import annotations

import imaplib
import re
import threading
import time
from email import message_from_bytes
from email.header import decode_header, make_header
from typing import Any

from .config import Config
from .progress import Progress

_QODER_CODE_RE = re.compile(r"\b(\d{6})\b")
_claimed_codes: set[str] = set()
_claimed_lock = threading.Lock()


def _decode(value: str) -> str:
    try:
        return str(make_header(decode_header(value)))
    except Exception:
        return value or ""


def extract_code(subject: str, body: str) -> str | None:
    for m in _QODER_CODE_RE.finditer(subject or ""):
        return m.group(1)
    plain = re.sub(r"<style[\s\S]*?</style>", " ", body or "", flags=re.I)
    plain = re.sub(r"<script[\s\S]*?</script>", " ", plain, flags=re.I)
    plain = re.sub(r"<[^>]+>", " ", plain)
    for m in _QODER_CODE_RE.finditer(plain):
        return m.group(1)
    return None


def matches_target(msg: Any, target_email: str) -> bool:
    target = (target_email or "").lower()
    if not target:
        return True
    for hdr in ("To", "Delivered-To", "X-Original-To", "Cc"):
        if target in _decode(msg.get(hdr) or "").lower():
            return True
    return False


def _body_text(msg: Any) -> str:
    if msg.is_multipart():
        for part in msg.walk():
            if part.get_content_type() == "text/plain":
                payload = part.get_payload(decode=True)
                if payload:
                    return payload.decode("utf-8", errors="replace")
        for part in msg.walk():
            if part.get_content_type() == "text/html":
                payload = part.get_payload(decode=True)
                if payload:
                    return payload.decode("utf-8", errors="replace")
        return ""
    payload = msg.get_payload(decode=True)
    return payload.decode("utf-8", errors="replace") if payload else ""


def _connect(cfg: Config, prog: Progress) -> imaplib.IMAP4_SSL | None:
    try:
        mail = imaplib.IMAP4_SSL(cfg.imap_host, cfg.imap_port)
        mail.login(cfg.imap_user, cfg.imap_pass)
        mail.select("INBOX")
        return mail
    except Exception as exc:
        prog.log(f"IMAP connect failed: {exc}", "ERR", step="otp")
        return None


def read_code(
    cfg: Config,
    target_email: str,
    prog: Progress,
    timeout: int = 180,
    since_ts: float = 0.0,
) -> str | None:
    if not cfg.imap_configured:
        prog.log("IMAP not configured", "ERR", email=target_email, step="otp")
        return None
    deadline = time.time() + timeout
    while time.time() < deadline:
        mail = _connect(cfg, prog)
        if mail is None:
            time.sleep(5)
            continue
        try:
            _, data = mail.search(None, '(FROM "qoder")')
            ids = (data[0] or b"").split()
            if not ids:
                _, data = mail.search(None, '(SUBJECT "code")')
                ids = (data[0] or b"").split()
            for num in reversed(ids[-30:]):
                try:
                    _, msg_data = mail.fetch(num, "(RFC822)")
                    msg = message_from_bytes(msg_data[0][1])
                    if not matches_target(msg, target_email):
                        continue
                    code = extract_code(_decode(msg.get("Subject") or ""), _body_text(msg))
                    if not code:
                        continue
                    with _claimed_lock:
                        if code in _claimed_codes:
                            continue
                        _claimed_codes.add(code)
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
