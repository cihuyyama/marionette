from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

from .browser import close_session, launch_camoufox
from .config import Config, load_config
from .eligibility import inject_eligibility
from .inject import dudul_inject
from .pat import exchange_pat, fetch_quota
from .progress import Progress


async def _inject_eligibility_reason(pat: str, cfg: Config, prog: Progress, display: str) -> str | None:
    try:
        exchanged = await exchange_pat(pat, cfg)
        sot = exchanged.get("securityOauthToken") or ""
        if not sot:
            return "quota unverifiable (PAT exchange returned no token)"
        quota = await fetch_quota(sot, cfg)
    except Exception as exc:
        return f"quota unverifiable ({type(exc).__name__})"
    eligible, reason = inject_eligibility(quota)
    return None if eligible else reason


def _mask_pat(pat: str) -> str:
    p = (pat or "").strip()
    if len(p) <= 10:
        return "****"
    return f"{p[:6]}…{p[-4:]} (len={len(p)})"


def _load_pats_file(path: str | Path) -> list[dict[str, str]]:
    raw = Path(path).read_text(encoding="utf-8")
    data = json.loads(raw)
    if isinstance(data, dict) and isinstance(data.get("accounts"), list):
        data = data["accounts"]
    if not isinstance(data, list):
        raise ValueError("pats file must be a JSON array (or {accounts:[]})")
    out: list[dict[str, str]] = []
    for i, item in enumerate(data):
        if isinstance(item, str):
            pat = item.strip()
            if not pat:
                continue
            out.append({"pat": pat, "email": "", "account_id": "", "label": ""})
            continue
        if not isinstance(item, dict):
            raise ValueError(f"pats[{i}] must be object or string")
        pat = str(
            item.get("pat")
            or item.get("personalToken")
            or item.get("personal_token")
            or ""
        ).strip()
        if not pat:
            continue
        out.append(
            {
                "pat": pat,
                "email": str(item.get("email") or "").strip(),
                "account_id": str(item.get("account_id") or item.get("id") or "").strip(),
                "label": str(item.get("label") or "").strip(),
            }
        )
    return out


async def run_inject_only(
    pat: str,
    cfg: Config | None = None,
    *,
    email: str = "",
    label: str = "",
    headless: bool | None = None,
    prog: Progress | None = None,
    reuse_session: dict[str, Any] | None = None,
) -> dict[str, Any]:
    cfg = cfg or load_config()
    from dataclasses import replace

    overrides: dict[str, Any] = {"dudul_inject": True}
    if headless is not None:
        overrides["headless"] = bool(headless)
    if cfg.inject_settle_secs and cfg.inject_settle_secs > 15:
        overrides["inject_settle_secs"] = 0
    cfg = replace(cfg, **overrides)

    pat = (pat or "").strip()
    if not pat:
        return {"ok": False, "reason": "missing pat", "email": email or None}
    if not (cfg.dudul_access_key or "").strip():
        return {
            "ok": False,
            "reason": "missing QODER_DUDUL_ACCESS_KEY",
            "email": email or None,
        }

    display = (email or label or "pat").strip() or "pat"
    owns_prog = prog is None
    if prog is None:
        prog = Progress(
            ui=cfg.ui,
            debug=cfg.debug,
            json_progress=cfg.json_progress,
            total=1,
        )
    prog.log(
        f"inject-only {_mask_pat(pat)} headless={cfg.headless}",
        "INFO",
        email=display,
        step="inject",
    )

    skip_reason = await _inject_eligibility_reason(pat, cfg, prog, display)
    if skip_reason:
        prog.log(f"inject skipped — {skip_reason}", "WAIT", email=display, step="inject")
        return {
            "ok": False,
            "skipped": True,
            "reason": skip_reason,
            "email": email or None,
            "label": label or None,
            "mode": "inject_only",
        }

    session = reuse_session
    close_owned = False
    try:
        if session is None:
            session = await launch_camoufox(cfg, prog)
            close_owned = True
        page = session["page"]
        result = await dudul_inject(page, pat, cfg, prog, display)
        out: dict[str, Any] = {
            "ok": bool(result.get("ok")),
            "email": email or None,
            "label": label or None,
            "mode": "inject_only",
            "url": result.get("url") or cfg.dudul_url,
            "key_masked": result.get("key_masked"),
        }
        for k in (
            "package",
            "tier",
            "credits_hint",
            "detail",
            "fatal",
            "fatal_code",
            "attempt",
            "attempts",
            "plan",
            "user_type",
            "trial_granted",
            "ultimate_claimed",
            "credits_total",
            "credits_remaining",
            "ultimate_limit",
            "ultimate_remaining",
            "ultimate_activity",
            "key_credits_left",
            "uid",
            "umid",
            "api_status",
        ):
            if result.get(k) is not None:
                out[k] = result.get(k)
        if result.get("ok"):
            if owns_prog:
                prog.ok += 1
            pkg = result.get("package") or result.get("tier")
            hint = result.get("credits_hint")
            if pkg and hint is not None:
                prog.log(
                    f"inject-only success ({pkg}, ~{hint})",
                    "OK",
                    email=display,
                    step="done",
                )
            elif pkg:
                prog.log(
                    f"inject-only success ({pkg})",
                    "OK",
                    email=display,
                    step="done",
                )
            else:
                prog.log("inject-only success", "OK", email=display, step="done")
        else:
            out["reason"] = result.get("reason") or "inject failed"
            out["skipped"] = bool(result.get("skipped"))
            if owns_prog:
                prog.fail += 1
            prog.log(
                f"inject-only failed: {out['reason']}",
                "ERR",
                email=display,
                step="done",
            )
        return out
    except Exception as exc:
        if owns_prog:
            prog.fail += 1
        prog.log(f"inject-only error: {exc}", "ERR", email=display, step="done")
        return {
            "ok": False,
            "reason": str(exc),
            "email": email or None,
            "mode": "inject_only",
        }
    finally:
        if close_owned:
            await close_session(session)


async def run_inject_bulk(
    items: list[dict[str, str]],
    cfg: Config | None = None,
    *,
    headless: bool | None = None,
) -> dict[str, Any]:
    cfg = cfg or load_config()
    from dataclasses import replace

    overrides: dict[str, Any] = {
        "dudul_inject": True,
        "inject_settle_secs": 0,
    }
    if headless is not None:
        overrides["headless"] = bool(headless)
    cfg = replace(cfg, **overrides)

    if not (cfg.dudul_access_key or "").strip():
        return {
            "ok": False,
            "reason": "missing QODER_DUDUL_ACCESS_KEY",
            "mode": "inject_bulk",
            "total": len(items),
            "ok_count": 0,
            "fail_count": len(items),
            "results": [],
        }

    total = len(items)
    prog = Progress(
        ui=cfg.ui,
        debug=cfg.debug,
        json_progress=cfg.json_progress,
        total=max(total, 1),
    )
    prog.log(
        f"inject-bulk start total={total} headless={cfg.headless}",
        "INFO",
        step="start",
    )

    results: list[dict[str, Any]] = []
    ok_count = 0
    fail_count = 0
    session = None

    try:
        import asyncio

        prog.log("inject-bulk launch camoufox once", "INFO", step="browser")
        session = await launch_camoufox(cfg, prog)
        page = session["page"]
        try:
            prog.log(f"inject-bulk open {cfg.dudul_url}", "INFO", step="navigate")
            await page.goto(cfg.dudul_url, wait_until="domcontentloaded", timeout=45_000)
            await page.wait_for_timeout(1500)
        except Exception as exc:
            prog.log(f"inject-bulk first navigate: {exc}", "WAIT", step="navigate")

        for idx, item in enumerate(items, start=1):
            pat = (item.get("pat") or "").strip()
            email = (item.get("email") or "").strip()
            account_id = (item.get("account_id") or "").strip()
            label = (item.get("label") or "").strip()
            display = email or label or account_id or f"#{idx}"
            prog.log(
                f"inject-bulk [{idx}/{total}] {display}",
                "INFO",
                email=display,
                step="inject",
            )
            one = await run_inject_only(
                pat,
                cfg,
                email=email,
                label=label or account_id,
                headless=cfg.headless,
                prog=prog,
                reuse_session=session,
            )
            one["account_id"] = account_id or None
            one["index"] = idx
            one["mode"] = "inject_bulk_item"
            if one.get("ok"):
                ok_count += 1
                prog.ok += 1
            else:
                fail_count += 1
                prog.fail += 1
            results.append(one)
            print(
                json.dumps(
                    {
                        "type": "inject_account_result",
                        **one,
                        "bulk_index": idx,
                        "bulk_total": total,
                        "ok_count": ok_count,
                        "fail_count": fail_count,
                    },
                    ensure_ascii=False,
                ),
                flush=True,
            )

            fatal = bool(one.get("fatal")) or one.get("fatal_code") == "dudul_access_key_exhausted"
            reason_l = str(one.get("reason") or "").lower()
            if not fatal and "no credit left" in reason_l and "key" in reason_l:
                fatal = True
                one["fatal"] = True
                one["fatal_code"] = "dudul_access_key_exhausted"

            if fatal:
                remaining = total - idx
                abort_reason = (
                    one.get("reason")
                    or "No credit left on this key."
                )
                prog.log(
                    f"inject-bulk abort: dudul access key exhausted "
                    f"at {idx}/{total} ({remaining} remaining skipped) — {abort_reason}",
                    "ERR",
                    email=display,
                    step="abort",
                )
                for j, skipped in enumerate(items[idx:], start=idx + 1):
                    skip_email = (skipped.get("email") or "").strip()
                    skip_id = (skipped.get("account_id") or "").strip()
                    skip_row = {
                        "ok": False,
                        "skipped": True,
                        "fatal": True,
                        "fatal_code": "dudul_access_key_exhausted",
                        "reason": f"aborted: dudul access key exhausted ({abort_reason})",
                        "email": skip_email or None,
                        "account_id": skip_id or None,
                        "index": j,
                        "mode": "inject_bulk_item",
                    }
                    fail_count += 1
                    prog.fail += 1
                    results.append(skip_row)
                    print(
                        json.dumps(
                            {
                                "type": "inject_account_result",
                                **skip_row,
                                "bulk_index": j,
                                "bulk_total": total,
                                "ok_count": ok_count,
                                "fail_count": fail_count,
                            },
                            ensure_ascii=False,
                        ),
                        flush=True,
                    )
                break

            if idx < total:
                await asyncio.sleep(0.8)
    finally:
        await close_session(session)

    aborted = any(
        r.get("fatal_code") == "dudul_access_key_exhausted" for r in results
    )
    summary = {
        "ok": ok_count > 0,
        "partial": fail_count > 0 and ok_count > 0,
        "mode": "inject_bulk",
        "total": total,
        "ok_count": ok_count,
        "fail_count": fail_count,
        "aborted": aborted,
        "fatal_code": "dudul_access_key_exhausted" if aborted else None,
        "results": [
            {
                "ok": r.get("ok"),
                "email": r.get("email"),
                "account_id": r.get("account_id"),
                "reason": r.get("reason"),
                "skipped": r.get("skipped"),
                "fatal": r.get("fatal"),
                "fatal_code": r.get("fatal_code"),
                "package": r.get("package"),
                "tier": r.get("tier"),
                "credits_hint": r.get("credits_hint"),
                "detail": r.get("detail"),
            }
            for r in results
        ],
    }
    if total == 0:
        summary["ok"] = False
        summary["reason"] = "no pats"
    elif ok_count == 0 and aborted:
        summary["ok"] = False
        summary["reason"] = "dudul access key exhausted (no credit left on this key)"
    elif ok_count == 0:
        summary["ok"] = False
        summary["reason"] = "all injects failed"
    elif aborted:
        summary["reason"] = (
            f"partial + aborted: {ok_count} ok, {fail_count} failed/skipped "
            f"(dudul access key exhausted)"
        )
    elif fail_count > 0:
        summary["reason"] = f"partial: {ok_count} ok, {fail_count} failed"
    else:
        summary["reason"] = f"all {ok_count} ok"

    prog.log(
        f"inject-bulk done ok={ok_count} fail={fail_count} total={total}"
        + (" aborted=dudul_key" if aborted else ""),
        "OK" if ok_count > 0 else "ERR",
        step="done",
    )
    return summary


def main_inject_only(argv: list[str] | None = None) -> int:
    import argparse
    import asyncio
    from dataclasses import replace

    p = argparse.ArgumentParser(
        prog="python -m qoder_farm --inject-only",
        description="Dudul inject for existing PAT(s) (activate Pro Trial)",
    )
    p.add_argument("--pat", default=None, help="Qoder personal token (pt-…)")
    p.add_argument(
        "--pats-file",
        default=None,
        help="JSON array of {pat,email,account_id} for bulk inject (one job)",
    )
    p.add_argument("--email", default="", help="Label for logs (single mode)")
    p.add_argument("--label", default="", help="Extra label")
    p.add_argument(
        "--headless",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Override QODER_HEADLESS",
    )
    p.add_argument(
        "--settle",
        type=int,
        default=None,
        help="Seconds before inject (default 0 for inject-only)",
    )
    p.add_argument(
        "--json-progress",
        action="store_true",
        help="NDJSON progress on stdout",
    )
    p.add_argument(
        "--no-proxy",
        action="store_true",
        help="Force direct connection; ignore all proxy env/file sources",
    )
    p.add_argument(
        "--proxy-file",
        default=None,
        help="Proxy list file (one URL per line; rotates per browser launch)",
    )
    p.add_argument(
        "--json-result",
        action="store_true",
        default=True,
        help="Print final result JSON line (default on)",
    )
    args = p.parse_args(argv)

    if not args.pat and not args.pats_file:
        p.error("require --pat or --pats-file")

    cfg = load_config()
    overrides: dict[str, Any] = {"dudul_inject": True, "inject_settle_secs": 0}
    if args.headless is not None:
        overrides["headless"] = bool(args.headless)
    if args.settle is not None:
        overrides["inject_settle_secs"] = max(0, int(args.settle))
    if args.json_progress:
        overrides["json_progress"] = True
    if args.no_proxy:
        overrides["no_proxy"] = True
    if args.proxy_file:
        overrides["proxy_file"] = str(args.proxy_file)
    cfg = replace(cfg, **overrides)

    if args.pats_file:
        try:
            items = _load_pats_file(args.pats_file)
        except Exception as exc:
            print(json.dumps({"type": "inject_result", "ok": False, "reason": str(exc)}), flush=True)
            print(f"inject-bulk load failed: {exc}", file=sys.stderr)
            return 1
        result = asyncio.run(run_inject_bulk(items, cfg, headless=cfg.headless))
    else:
        result = asyncio.run(
            run_inject_only(
                args.pat or "",
                cfg,
                email=args.email or "",
                label=args.label or "",
            )
        )

    line = {
        "type": "inject_result",
        **result,
    }
    print(json.dumps(line, ensure_ascii=False), flush=True)
    if result.get("ok"):
        if result.get("partial"):
            print(
                f"inject-only partial: {result.get('reason')}",
                file=sys.stderr,
            )
        return 0
    print(
        f"inject-only failed: {result.get('reason')}",
        file=sys.stderr,
    )
    return 1
