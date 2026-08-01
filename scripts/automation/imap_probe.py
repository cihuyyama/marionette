from __future__ import annotations

import argparse
import imaplib
import os
import sys
from email import message_from_bytes
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from grok_farm.register import _extract_otp, _matches_target  # noqa: E402


def _load_env_file(path: Path) -> None:
    if not path.is_file():
        print(f"[probe] env file not found: {path}", flush=True)
        return
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, _, v = line.partition("=")
        os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


def _mask(value: str) -> str:
    if not value:
        return "<empty>"
    if "@" in value:
        name, _, domain = value.partition("@")
        head = name[:2] if len(name) > 2 else name[:1]
        return f"{head}***@{domain}"
    return f"{value[:2]}***"


def main() -> int:
    ap = argparse.ArgumentParser(description="Standalone IMAP OTP probe (no secrets printed)")
    ap.add_argument("--env", default=str(Path(__file__).resolve().parents[2].parent / "grok-farm" / ".env"))
    ap.add_argument("--target", help="target catch-all email to match (optional)")
    ap.add_argument("--limit", type=int, default=10, help="most-recent messages to scan")
    args = ap.parse_args()

    _load_env_file(Path(args.env))

    host = (os.environ.get("GROK_IMAP_HOST") or "imap.gmail.com").strip()
    port = int((os.environ.get("GROK_IMAP_PORT") or "993").strip() or "993")
    user = (os.environ.get("GROK_IMAP_USER") or "").strip()
    raw_pass = os.environ.get("GROK_IMAP_PASS") or ""
    stripped = raw_pass.replace(" ", "")

    print(f"[probe] host={host} port={port} user={_mask(user)}", flush=True)
    print(f"[probe] pass had_spaces={' ' in raw_pass} len_raw={len(raw_pass)} len_stripped={len(stripped)}", flush=True)
    if not (host and user and stripped):
        print("[probe] FAIL: incomplete IMAP config", flush=True)
        return 2

    try:
        mail = imaplib.IMAP4_SSL(host, port)
        mail.login(user, stripped)
        mail.select("INBOX")
        print("[probe] login OK", flush=True)
    except Exception as exc:
        print(f"[probe] login FAILED: {exc}", flush=True)
        return 3

    try:
        typ, data = mail.search(None, '(FROM "x.ai")')
        ids = (data[0] or b"").split()
        used = 'FROM "x.ai"'
        if not ids:
            typ, data = mail.search(None, '(SUBJECT "confirmation code")')
            ids = (data[0] or b"").split()
            used = 'SUBJECT "confirmation code"'
        print(f"[probe] search={used} matched={len(ids)} message(s)", flush=True)

        for num in reversed(ids[-args.limit :]):
            typ, msg_data = mail.fetch(num, "(RFC822)")
            msg = message_from_bytes(msg_data[0][1])
            subject = msg.get("Subject") or ""
            to = msg.get("To") or ""
            dto = msg.get("Delivered-To") or ""
            body = ""
            if msg.is_multipart():
                for part in msg.walk():
                    if part.get_content_type() == "text/plain":
                        body = part.get_payload(decode=True).decode("utf-8", errors="replace")
                        break
                if not body:
                    for part in msg.walk():
                        if part.get_content_type() == "text/html":
                            body = part.get_payload(decode=True).decode("utf-8", errors="replace")
                            break
            else:
                body = msg.get_payload(decode=True).decode("utf-8", errors="replace")

            code = _extract_otp(subject, body)
            match = _matches_target(msg, args.target) if args.target else "n/a"
            print(
                f"[probe]  - subj={subject!r} to={_mask(to.strip())} dto={_mask(dto.strip())} "
                f"target_match={match} otp={code}",
                flush=True,
            )
    finally:
        try:
            mail.logout()
        except Exception:
            pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
