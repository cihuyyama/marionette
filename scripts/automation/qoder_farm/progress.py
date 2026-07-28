from __future__ import annotations

import json
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any


def _now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


@dataclass
class Progress:
    """
    Human log lines + optional machine-readable NDJSON for Marionette dashboard.

    When json_progress=True, every event also emits one stdout line:
      {"type":"farm","ts":"...","level":"STEP","step":"sso","email":"a***@x","msg":"...","ok":0,"fail":0}
    """

    ui: str = "log"
    debug: bool = False
    json_progress: bool = False
    total: int = 0
    started: float = field(default_factory=time.monotonic)
    ok: int = 0
    fail: int = 0
    current_email: str = ""
    current_step: str = ""

    def _pfx(self, level: str) -> str:
        return {
            "INFO": "*",
            "OK": "+",
            "ERR": "!",
            "DBG": ".",
            "WAIT": "~",
            "STEP": ">",
        }.get(level, "*")

    def _emit_json(self, level: str, msg: str, email: str = "", step: str = "") -> None:
        if not self.json_progress:
            return
        payload: dict[str, Any] = {
            "type": "farm",
            "ts": _now_iso(),
            "level": level,
            "msg": msg,
            "email": email or self.current_email,
            "step": step or self.current_step,
            "ok": self.ok,
            "fail": self.fail,
            "total": self.total,
            "elapsed_s": round(time.monotonic() - self.started, 1),
        }
        print(json.dumps(payload, ensure_ascii=False), flush=True)

    def log(self, msg: str, level: str = "INFO", email: str = "", step: str = "") -> None:
        if level == "DBG" and not self.debug:
            return
        if self.json_progress:
            self._emit_json(level, msg, email=email, step=step)
            return
        who = f"[{email}] " if email else ""
        line = f"{self._pfx(level)} {who}{msg}"
        print(line, flush=True)

    def step(self, email: str, step: str, detail: str = "") -> None:
        self.current_email = email
        self.current_step = step
        extra = f" - {detail}" if detail else ""
        self.log(f"{step}{extra}", "STEP", email=email, step=step)

    def mark_ok(self, email: str, msg: str = "ok") -> None:
        self.ok += 1
        self.log(msg, "OK", email=email, step=self.current_step or "done")

    def mark_fail(self, email: str, msg: str) -> None:
        self.fail += 1
        self.log(msg, "ERR", email=email, step=self.current_step or "error")

    def account_ok(
        self,
        *,
        email: str,
        path: str = "",
        masked_email: str = "",
    ) -> None:
        label = masked_email or mask_email(email) or email
        if self.json_progress:
            payload: dict[str, Any] = {
                "type": "farm",
                "event": "account_ok",
                "ts": _now_iso(),
                "level": "OK",
                "msg": "account ready for import",
                "email": email,
                "email_masked": label,
                "step": "import",
                "ok": self.ok,
                "fail": self.fail,
                "total": self.total,
                "elapsed_s": round(time.monotonic() - self.started, 1),
            }
            if path:
                payload["path"] = path
            print(json.dumps(payload, ensure_ascii=False), flush=True)
            return
        self.log(f"account_ok {label}", "OK", email=label, step="import")

    def summary(self) -> None:
        elapsed = time.monotonic() - self.started
        msg = f"done ok={self.ok} fail={self.fail} elapsed={elapsed:.0f}s"
        if self.json_progress:
            self._emit_json("INFO", msg, step="summary")
            print(
                json.dumps(
                    {
                        "type": "farm",
                        "event": "finished",
                        "ts": _now_iso(),
                        "ok": self.ok,
                        "fail": self.fail,
                        "total": self.total,
                        "elapsed_s": round(elapsed, 1),
                    },
                    ensure_ascii=False,
                ),
                flush=True,
            )
        else:
            print(f"\n== {msg} ==", flush=True)


def mask_email(email: str) -> str:
    if "@" not in email:
        return email[:2] + "***" if email else ""
    local, _, domain = email.partition("@")
    if len(local) <= 2:
        return f"{local[0]}***@{domain}"
    return f"{local[0]}***{local[-1]}@{domain}"


def parse_account_line(raw: str) -> tuple[str, str] | None:
    line = raw.strip()
    if not line or line.startswith("#"):
        return None
    if "|" in line:
        email, _, password = line.partition("|")
    elif ":" in line:
        at = line.find("@")
        if at < 0:
            return None
        colon = line.find(":", at)
        if colon < 0:
            return None
        email, password = line[:colon], line[colon + 1 :]
    else:
        return None
    email, password = email.strip(), password.strip()
    if not email or not password or "@" not in email:
        return None
    return email, password


def load_accounts(path: str | None, positional: list[str]) -> list[tuple[str, str]]:
    rows: list[tuple[str, str]] = []
    for p in positional:
        parsed = parse_account_line(p)
        if parsed:
            rows.append(parsed)
    if path:
        text = open(path, encoding="utf-8").read()
        for line in text.splitlines():
            parsed = parse_account_line(line)
            if parsed:
                rows.append(parsed)
    seen: set[str] = set()
    out: list[tuple[str, str]] = []
    for email, password in rows:
        key = email.lower()
        if key in seen:
            continue
        seen.add(key)
        out.append((email, password))
    return out
