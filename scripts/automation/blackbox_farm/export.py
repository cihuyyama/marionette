"""9Router-shaped export for blackbox_farm (mirrors grok_farm/export.py).

Shape matches what `just import-json` / marionette-import expects:
one providerConnections array row per account. The password is kept — it is
needed to log back in and recreate keys later; marionette-import ignores
unknown fields.
"""
from __future__ import annotations

import json
import os
import tempfile
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def _now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def connection_from_result(result: dict[str, Any]) -> dict[str, Any] | None:
    """Build one 9Router-shaped providerConnection for marionette-import."""
    api_key = str(result.get("apiKey") or result.get("api_key") or "")
    if not api_key:
        return None

    email = str(result.get("email") or "")
    now = _now_iso()
    password = str(result.get("password") or "")

    conn: dict[str, Any] = {
        "id": result.get("id") or str(uuid.uuid4()),
        "provider": "blackbox",
        "email": email,
        "name": email or str(result.get("name") or ""),
        "displayName": email,
        "isActive": True,
        "priority": int(result.get("priority") or 0),
        "createdAt": result.get("createdAt") or now,
        "updatedAt": now,
        "apiKey": api_key,
        "farmMeta": {
            "farm": "blackbox-farm",
            "farmedAt": now,
        },
    }
    if password:
        conn["password"] = password
    return conn


def _atomic_write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=".tmp_", suffix=path.suffix)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(text)
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp):
            try:
                os.remove(tmp)
            except OSError:
                pass


def write_backup(
    results: list[dict[str, Any]],
    path: Path,
    *,
    append: bool = True,
) -> tuple[int, Path]:
    """Write `{ "providerConnections": [...] }` JSON for:
      just import-json <file>
      cargo run --bin marionette-import -- --file <file>

    Append merges by provider:blackbox-email (same dedupe logic as
    grok_farm/export.py so mixed backups stay intact).
    """
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)

    existing: list[Any] = []
    if append and path.is_file():
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
            if isinstance(raw, dict) and isinstance(raw.get("providerConnections"), list):
                existing = list(raw["providerConnections"])
            elif isinstance(raw, list):
                existing = raw
        except Exception:
            existing = []

    by_email: dict[str, dict[str, Any]] = {}
    for item in existing:
        if not isinstance(item, dict):
            continue
        # Keep non-blackbox rows intact when re-writing mixed backups
        em = str(item.get("email") or "").lower()
        key = em or str(item.get("id") or uuid.uuid4())
        # Prefer provider+email for uniqueness across mixed files
        prov = str(item.get("provider") or "")
        if em and prov:
            key = f"{prov}:{em}"
        elif em:
            key = em
        by_email[key] = item

    added = 0
    for result in results:
        if not result.get("ok"):
            continue
        conn = connection_from_result(result)
        if not conn:
            continue
        em = str(conn.get("email") or "").lower()
        key = f"blackbox:{em}" if em else str(conn["id"])
        by_email[key] = conn
        added += 1

    connections = list(by_email.values())
    payload = {
        "providerConnections": connections,
        "exportedAt": _now_iso(),
        "source": "marionette/scripts/automation/blackbox_farm",
        "count": len(connections),
    }
    _atomic_write_text(path, json.dumps(payload, indent=2, ensure_ascii=False) + "\n")
    return added, path
