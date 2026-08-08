from __future__ import annotations

import json
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

    Copied from grok_farm/progress.py (packages are intentionally decoupled);
    emits provider "blackbox". When json_progress=True, every event also emits
    one stdout line:
      {"type":"farm","provider":"blackbox","ts":"...","level":"STEP","step":"create_key",...}
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
            "WARN": "!",
        }.get(level, "*")

    def _emit_json(self, level: str, msg: str, email: str = "", step: str = "") -> None:
        if not self.json_progress:
            return
        payload: dict[str, Any] = {
            "type": "farm",
            "provider": "blackbox",
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
                "provider": "blackbox",
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
                        "provider": "blackbox",
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
