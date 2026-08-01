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
    """
    Build one 9Router-shaped providerConnection for marionette-import.

    import_util::build_grok_data expects top-level:
      accessToken (required), refreshToken, idToken, clientId, expiresAt, expiresIn, scope
    """
    access = result.get("accessToken") or result.get("access_token") or ""
    if not access:
        return None

    email = result.get("email") or ""
    now = _now_iso()
    refresh = result.get("refreshToken") or result.get("refresh_token") or ""
    id_token = result.get("idToken") or result.get("id_token") or ""
    client_id = (
        result.get("clientId")
        or result.get("client_id")
        or "b1a00492-073a-47ea-816f-4c329264a828"
    )
    expires_at = result.get("expiresAt") or result.get("expires_at") or ""
    expires_in = result.get("expiresIn") or result.get("expires_in")
    scope = result.get("scope") or ""

    conn: dict[str, Any] = {
        "id": result.get("id") or str(uuid.uuid4()),
        "provider": "grok-cli",
        "email": email,
        "name": email or result.get("name") or "",
        "displayName": email,
        "isActive": True,
        "priority": int(result.get("priority") or 0),
        "createdAt": result.get("createdAt") or now,
        "updatedAt": now,
        "accessToken": access,
        "clientId": client_id,
        "farmMeta": {
            "farm": "grok-farm",
            "authMode": result.get("auth_mode") or result.get("authMode") or "oidc",
            "verified": bool(result.get("verified", True)),
            "farmedAt": now,
        },
    }
    if refresh:
        conn["refreshToken"] = refresh
    if id_token:
        conn["idToken"] = id_token
    if expires_at:
        conn["expiresAt"] = expires_at
    if expires_in is not None:
        try:
            conn["expiresIn"] = int(expires_in)
        except (TypeError, ValueError):
            pass
    if scope:
        conn["scope"] = scope
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
    """
    Write `{ "providerConnections": [...] }` JSON for:
      just import-json <file>
      cargo run --bin marionette-import -- --file <file>
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
        # Keep non-grok rows intact when re-writing mixed backups
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
        key = f"grok-cli:{em}" if em else str(conn["id"])
        by_email[key] = conn
        added += 1

    connections = list(by_email.values())
    payload = {
        "providerConnections": connections,
        "exportedAt": _now_iso(),
        "source": "marionette/scripts/automation/grok_farm",
        "count": len(connections),
    }
    _atomic_write_text(path, json.dumps(payload, indent=2, ensure_ascii=False) + "\n")
    return added, path


def write_failures(failures: list[dict[str, Any]], path: Path) -> None:
    if not failures:
        return
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps({"failures": failures, "at": _now_iso()}, indent=2, ensure_ascii=False)
        + "\n",
        encoding="utf-8",
    )


def emails_in_output(path: Path) -> set[str]:
    """Emails already present in output JSON (any provider row)."""
    found: set[str] = set()
    path = Path(path)
    if not path.is_file():
        return found
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return found
    rows: list[Any] = []
    if isinstance(data, list):
        rows = data
    elif isinstance(data, dict):
        for key in ("providerConnections", "connections", "accounts"):
            v = data.get(key)
            if isinstance(v, list):
                rows = v
                break
    for row in rows:
        if not isinstance(row, dict):
            continue
        # Prefer grok-cli only for skip-existing of this farm
        prov = str(row.get("provider") or "")
        if prov and prov not in ("grok-cli", "grok"):
            continue
        email = row.get("email") or row.get("name") or ""
        if isinstance(email, str) and "@" in email:
            found.add(email.strip().lower())
    return found


def pending_path_for(output: Path) -> Path:
    output = Path(output)
    return output.with_name(output.stem + ".pending.jsonl")


def append_pending(row: dict[str, Any], output: Path) -> Path:
    path = pending_path_for(output)
    path.parent.mkdir(parents=True, exist_ok=True)
    line = json.dumps({**row, "pendingAt": _now_iso()}, ensure_ascii=False)
    with path.open("a", encoding="utf-8") as fh:
        fh.write(line + "\n")
    return path


def load_pending(output: Path) -> list[dict[str, Any]]:
    path = pending_path_for(output)
    if not path.is_file():
        return []
    rows: list[dict[str, Any]] = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        raw = line.strip()
        if not raw:
            continue
        try:
            row = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if isinstance(row, dict) and row.get("email"):
            rows.append(row)
    return rows


def rewrite_pending(rows: list[dict[str, Any]], output: Path) -> Path:
    path = pending_path_for(output)
    if not rows:
        if path.is_file():
            try:
                path.unlink()
            except OSError:
                pass
        return path
    body = "\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n"
    _atomic_write_text(path, body)
    return path


def drop_pending_email(email: str, output: Path) -> Path:
    target = (email or "").strip().lower()
    remaining = [r for r in load_pending(output) if str(r.get("email") or "").lower() != target]
    return rewrite_pending(remaining, output)
