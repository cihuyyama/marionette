from __future__ import annotations

import json
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def _now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def connection_from_result(result: dict[str, Any]) -> dict[str, Any] | None:
    """
    Build one 9Router-shaped providerConnection for marionette-import.
    Requires personalToken. Returns None if incomplete.
    """
    pat = result.get("personalToken") or ""
    if not pat:
        return None

    email = result.get("email") or ""
    now = _now_iso()
    machine_id = result.get("machineId") or str(uuid.uuid4())
    sot = result.get("securityOauthToken") or ""
    refresh = result.get("refreshToken") or ""
    quota = result.get("quota") or {}
    inject = result.get("inject") or {}

    psd: dict[str, Any] = {
        "personalToken": pat,
        "machineId": machine_id,
        "machineType": result.get("machineType") or "5",
        "authMethod": result.get("authMethod") or "gsuite",
        "plan": (quota.get("plan") if isinstance(quota, dict) else None)
        or result.get("plan")
        or "Community",
    }
    if sot:
        psd["securityOauthToken"] = sot
    if result.get("userId"):
        psd["userId"] = result["userId"]
    if result.get("organizationId") is not None:
        psd["organizationId"] = result.get("organizationId") or ""
    if result.get("machineToken"):
        psd["machineToken"] = result["machineToken"]
    if result.get("expireTime") is not None:
        psd["expireTime"] = result["expireTime"]

    farm_meta = {
        "farm": "qoder-farm",
        "injectOk": bool(inject.get("ok")),
        "injectSkipped": bool(inject.get("skipped")),
        "injectReason": inject.get("reason"),
        "farmedAt": now,
    }
    if isinstance(quota, dict) and quota:
        farm_meta["quotaRemaining"] = quota.get("quotaRemaining")
        farm_meta["quotaLimit"] = quota.get("quotaLimit")
        farm_meta["plan"] = quota.get("plan")

    conn: dict[str, Any] = {
        "id": result.get("id") or str(uuid.uuid4()),
        "provider": "qoder",
        "email": email,
        "name": email or result.get("name") or "",
        "displayName": email,
        "isActive": True,
        "priority": int(result.get("priority") or 0),
        "createdAt": result.get("createdAt") or now,
        "updatedAt": now,
        "providerSpecificData": psd,
        "farmMeta": farm_meta,
    }
    if sot:
        conn["accessToken"] = sot
    if refresh:
        conn["refreshToken"] = refresh
    if result.get("expiresAt"):
        conn["expiresAt"] = result["expiresAt"]

    return conn


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

    # index by email for upsert
    by_email: dict[str, dict[str, Any]] = {}
    for item in existing:
        if not isinstance(item, dict):
            continue
        em = str(item.get("email") or "").lower()
        if em:
            by_email[em] = item
        else:
            by_email[str(item.get("id") or uuid.uuid4())] = item

    added = 0
    for result in results:
        if not result.get("ok"):
            continue
        conn = connection_from_result(result)
        if not conn:
            continue
        em = str(conn.get("email") or "").lower()
        key = em or str(conn["id"])
        by_email[key] = conn
        added += 1

    connections = list(by_email.values())
    payload = {
        "providerConnections": connections,
        "exportedAt": _now_iso(),
        "source": "marionette/scripts/qoder-farm",
        "count": len(connections),
    }
    path.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
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
