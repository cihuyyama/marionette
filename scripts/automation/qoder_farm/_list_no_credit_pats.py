from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

DB = Path(__file__).resolve().parents[3] / "data" / "marionette.sqlite"


def pat_of(data_s: str) -> str | None:
    try:
        d = json.loads(data_s or "{}")
    except Exception:
        return None
    for k in ("personalToken", "personal_token", "pat"):
        v = d.get(k)
        if isinstance(v, str) and v.strip():
            return v.strip()
    psd = d.get("providerSpecificData")
    if isinstance(psd, dict):
        for k in ("personalToken", "personal_token", "pat"):
            v = psd.get(k)
            if isinstance(v, str) and v.strip():
                return v.strip()
    return None


def main() -> int:
    if not DB.is_file():
        print(f"missing db: {DB}", file=sys.stderr)
        return 1
    conn = sqlite3.connect(str(DB))
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        """
        SELECT id, email, is_active, quota_limit, quota_remaining, data
        FROM accounts
        WHERE provider = 'qoder'
        ORDER BY email
        """
    ).fetchall()

    out: list[str] = []
    missing = 0
    for r in rows:
        lim = int(r["quota_limit"] or 0)
        rem = int(r["quota_remaining"] or 0)
        if lim > 0 and rem > 0:
            continue
        p = pat_of(r["data"])
        if p:
            out.append(p)
        else:
            missing += 1

    print(f"# no-credit with PAT: {len(out)} (missing pat: {missing})", file=sys.stderr)
    for p in out:
        print(p)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
