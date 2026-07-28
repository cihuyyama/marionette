from __future__ import annotations

import json
import sqlite3
import ssl
import urllib.error
import urllib.request
from collections import Counter, defaultdict
from pathlib import Path

DB = Path(__file__).resolve().parents[3] / "data" / "marionette.sqlite"
URL = "https://openapi.qoder.sh/api/v2/quota/usage"
OUT = Path(__file__).resolve().parent / "results" / "quota_usage_probe.json"


def get_sot(data_s: str) -> str | None:
    try:
        d = json.loads(data_s or "{}")
    except Exception:
        return None
    for k in (
        "securityOauthToken",
        "security_oauth_token",
        "accessToken",
        "jobToken",
    ):
        v = d.get(k)
        if isinstance(v, str) and v.strip():
            return v.strip()
    psd = d.get("providerSpecificData")
    if isinstance(psd, dict):
        for k in (
            "securityOauthToken",
            "security_oauth_token",
            "accessToken",
            "jobToken",
        ):
            v = psd.get(k)
            if isinstance(v, str) and v.strip():
                return v.strip()
    return None


def sanitize(j: dict) -> dict:
    skip = {"userId"}
    out = {}
    for k, v in j.items():
        lk = str(k).lower()
        if k in skip or lk.endswith("token") or "oauth" in lk:
            continue
        out[k] = v
    return out


def main() -> int:
    if not DB.is_file():
        print(f"missing db: {DB}")
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
    print(f"accounts={len(rows)}")

    ctx = ssl.create_default_context()
    types: Counter[str] = Counter()
    samples: dict[str, list] = defaultdict(list)
    errors: list[dict] = []
    ok_rows: list[dict] = []

    for r in rows:
        email = r["email"] or r["id"]
        sot = get_sot(r["data"])
        if not sot:
            errors.append({"email": email, "error": "no sot"})
            continue
        req = urllib.request.Request(
            URL,
            headers={
                "Authorization": f"Bearer {sot}",
                "cosy-clienttype": "5",
                "cosy-version": "0.1.20",
                "User-Agent": "qodercli/0.1.20",
                "Accept": "application/json",
            },
        )
        try:
            with urllib.request.urlopen(req, context=ctx, timeout=25) as resp:
                body = resp.read().decode("utf-8", errors="replace")
                j = json.loads(body)
        except urllib.error.HTTPError as e:
            err_body = e.read().decode("utf-8", errors="replace")[:300]
            errors.append(
                {
                    "email": email,
                    "error": f"http {e.code}",
                    "body": err_body,
                }
            )
            continue
        except Exception as e:
            errors.append({"email": email, "error": f"{type(e).__name__}: {e}"})
            continue

        ut = str(j.get("userType") or j.get("user_type") or "(missing)")
        types[ut] += 1
        row = {
            "id": r["id"],
            "email": r["email"],
            "db_quota_limit": r["quota_limit"],
            "db_quota_remaining": r["quota_remaining"],
            "userType": j.get("userType"),
            "usageType": j.get("usageType"),
            "isQuotaExceeded": j.get("isQuotaExceeded"),
            "expiresAt": j.get("expiresAt"),
            "userQuota": j.get("userQuota"),
            "raw_keys": sorted(j.keys()),
            "sanitized": sanitize(j),
        }
        ok_rows.append(row)
        if len(samples[ut]) < 3:
            samples[ut].append(row)

    report = {
        "ok": len(ok_rows),
        "err": len(errors),
        "userType_counts": dict(types.most_common()),
        "samples_by_userType": {k: v for k, v in samples.items()},
        "errors_head": errors[:20],
        "all_ok": ok_rows,
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")

    print(f"ok={len(ok_rows)} err={len(errors)}")
    print("userType counts:")
    for k, v in types.most_common():
        print(f"  {v:3d}  {k}")
    print("--- samples ---")
    for ut, arr in samples.items():
        print(f"TYPE {ut}")
        for s in arr[:2]:
            print(
                json.dumps(
                    {
                        "email": s["email"],
                        "userType": s["userType"],
                        "usageType": s["usageType"],
                        "userQuota": s["userQuota"],
                        "keys": s["raw_keys"],
                        "extra": {
                            kk: vv
                            for kk, vv in (s.get("sanitized") or {}).items()
                            if kk
                            not in (
                                "userType",
                                "usageType",
                                "isQuotaExceeded",
                                "expiresAt",
                                "userQuota",
                            )
                        },
                    },
                    ensure_ascii=False,
                )[:900]
            )
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
