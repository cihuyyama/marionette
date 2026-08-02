from __future__ import annotations

import asyncio
import json
import traceback
from pathlib import Path
from typing import Any

from .browser import close_session, launch_camoufox
from .config import Config
from .export import write_backup, write_failures
from .google_sso import (
    click_qoder_google_button,
    drive_google_auth,
    is_sso_retryable_error,
)
from .inject import dudul_inject
from .pat import (
    approve_device_auth,
    create_pat,
    exchange_pat,
    fetch_quota,
)
from .progress import Progress, mask_email
from .session import ensure_qoder_session, recover_page_load_error


def _has_prior_credit(quota: dict[str, Any] | None) -> bool:
    # A fresh Free/Community account has never been granted a credit bucket:
    # quotaLimit == 0. Any positive limit means credits were already issued
    # (even if now 0 remaining / exhausted), so it must NOT be re-injected.
    if not quota:
        return False
    try:
        return float(quota.get("quotaLimit") or 0) > 0
    except (TypeError, ValueError):
        return False


def write_failed_accounts(
    failures: list[dict[str, Any]],
    password_by_email: dict[str, str],
    path: Path,
) -> int:
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    lines: list[str] = []
    seen: set[str] = set()
    for r in failures:
        email = str(r.get("email") or "").strip()
        if not email:
            continue
        key = email.lower()
        if key in seen:
            continue
        seen.add(key)
        password = password_by_email.get(key) or password_by_email.get(email) or ""
        if not password:
            continue
        lines.append(f"{email}|{password}")
    path.write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")
    return len(lines)


async def _screenshot(page: Any, cfg: Config, email: str, tag: str) -> None:
    try:
        cfg.screenshot_dir.mkdir(parents=True, exist_ok=True)
        safe = email.replace("@", "_at_").replace(".", "_")
        path = cfg.screenshot_dir / f"{safe}_{tag}.png"
        await page.screenshot(path=str(path), full_page=True)
    except Exception:
        pass


async def process_one(
    email: str,
    password: str,
    cfg: Config,
    prog: Progress,
    *,
    do_inject: bool,
    do_device_auth: bool,
    skip_exchange: bool,
    count_result: bool = True,
) -> dict[str, Any]:
    label = mask_email(email)
    result: dict[str, Any] = {
        "ok": False,
        "email": email,
        "personalToken": None,
        "securityOauthToken": None,
        "machineId": None,
        "inject": None,
        "quota": None,
        "error": None,
    }
    session: dict[str, Any] | None = None

    max_sso_attempts = 3
    try:
        prog.step(label, "browser", "launch camoufox")
        session = await launch_camoufox(cfg, prog)
        page = session["page"]

        sso_ok = False
        last_sso_err: Exception | None = None
        for sso_attempt in range(1, max_sso_attempts + 1):
            try:
                if sso_attempt > 1:
                    prog.log(
                        f"sso retry {sso_attempt}/{max_sso_attempts} (fresh browser)",
                        "WARN",
                        email=label,
                    )
                    await close_session(session)
                    session = None
                    await asyncio.sleep(1.2 + 0.8 * (sso_attempt - 1))
                    prog.step(label, "browser", f"relaunch camoufox (sso retry {sso_attempt})")
                    session = await launch_camoufox(cfg, prog)
                    page = session["page"]

                prog.step(
                    label,
                    "sso",
                    f"qoder sign-in (attempt {sso_attempt}/{max_sso_attempts})",
                )
                await page.goto(
                    cfg.sign_in_url, wait_until="domcontentloaded", timeout=45_000
                )
                await asyncio.sleep(1.5)
                try:
                    await recover_page_load_error(page)
                except Exception:
                    pass
                await click_qoder_google_button(page, prog, label)
                await drive_google_auth(page, email, password, prog)
                sso_ok = True
                break
            except Exception as exc:
                last_sso_err = exc
                if not is_sso_retryable_error(exc) or sso_attempt >= max_sso_attempts:
                    raise
                prog.log(
                    f"sso stuck → will retry: {exc}",
                    "WARN",
                    email=label,
                )
                try:
                    await _screenshot(
                        page, cfg, email, f"sso_stuck_{sso_attempt}"
                    )
                except Exception:
                    pass

        if not sso_ok:
            raise last_sso_err or RuntimeError("sso failed without error")

        prog.step(label, "session", "gate after SSO")
        await ensure_qoder_session(page)
        prog.log("session gate ok", "OK", email=label)

        pat = await create_pat(page, cfg, prog, label)
        result["personalToken"] = pat

        machine_id = None
        if do_device_auth:
            try:
                machine_id = await approve_device_auth(page, cfg, prog, label)
            except Exception as exc:
                prog.log(f"device auth skipped: {exc}", "WAIT", email=label)
                machine_id = None
        if machine_id:
            result["machineId"] = machine_id

        sot = ""
        refresh = ""
        if not skip_exchange:
            prog.step(label, "exchange", "PAT → jobToken")
            try:
                exchanged = await exchange_pat(pat, cfg)
                sot = exchanged.get("securityOauthToken") or ""
                refresh = exchanged.get("refreshToken") or ""
                result["securityOauthToken"] = sot
                result["refreshToken"] = refresh
                raw = exchanged.get("raw") or {}
                if isinstance(raw, dict):
                    if raw.get("user_id") or raw.get("userId"):
                        result["userId"] = raw.get("user_id") or raw.get("userId")
                    if raw.get("expire_time") or raw.get("expireTime"):
                        result["expireTime"] = raw.get("expire_time") or raw.get(
                            "expireTime"
                        )
                prog.log("PAT exchange ok", "OK", email=label)
            except Exception as exc:
                prog.log(f"exchange failed (PAT still saved): {exc}", "WAIT", email=label)

        if sot and not result.get("quota"):
            prog.step(label, "quota", "fetch usage")
            try:
                quota = await fetch_quota(sot, cfg)
                result["quota"] = quota
                if quota:
                    prog.log(
                        f"quota remaining={quota.get('quotaRemaining')} "
                        f"limit={quota.get('quotaLimit')} plan={quota.get('plan')}",
                        "INFO",
                        email=label,
                    )
            except Exception as exc:
                prog.log(f"quota fetch err: {exc}", "DBG", email=label)

        inject_info: dict[str, Any]
        if not (do_inject and cfg.dudul_inject):
            inject_info = {"ok": False, "skipped": True, "reason": "inject disabled"}
        elif cfg.inject_only_free and _has_prior_credit(result.get("quota")):
            q = result.get("quota") or {}
            reason = (
                f"has prior credit (limit={q.get('quotaLimit')} "
                f"remaining={q.get('quotaRemaining')} plan={q.get('plan')})"
            )
            prog.log(f"inject skipped — {reason}", "WAIT", email=label)
            inject_info = {"ok": False, "skipped": True, "reason": reason}
        else:
            inject_info = await dudul_inject(page, pat, cfg, prog, label)
        result["inject"] = inject_info

        result["ok"] = True
        result["authMethod"] = "gsuite"
        if count_result:
            prog.mark_ok(label, "farmed")
        else:
            prog.log("farmed", "OK", email=label)
        return result

    except Exception as exc:
        result["error"] = str(exc)
        if count_result:
            prog.mark_fail(label, str(exc))
        else:
            prog.log(str(exc), "ERR", email=label)
        if cfg.debug:
            prog.log(traceback.format_exc(), "DBG", email=label)
        if session and session.get("page"):
            await _screenshot(session["page"], cfg, email, "error")
        return result
    finally:
        await close_session(session)


def load_already_farmed_emails(
    *,
    output: Path,
    skip_emails_file: Path | None = None,
) -> set[str]:
    found: set[str] = set()
    if skip_emails_file and skip_emails_file.is_file():
        try:
            for line in skip_emails_file.read_text(
                encoding="utf-8", errors="replace"
            ).splitlines():
                raw = line.strip()
                if not raw or raw.startswith("#"):
                    continue
                email = raw.split("|", 1)[0].split(":", 1)[0].strip().lower()
                if email and "@" in email:
                    found.add(email)
        except OSError:
            pass
    if output.is_file():
        try:
            data = json.loads(output.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            data = None
        rows: list[Any] = []
        if isinstance(data, list):
            rows = data
        elif isinstance(data, dict):
            for key in ("providerConnections", "connections", "accounts"):
                v = data.get(key)
                if isinstance(v, list):
                    rows = v
                    break
            if not rows and isinstance(data.get("data"), list):
                rows = data["data"]
        for row in rows:
            if not isinstance(row, dict):
                continue
            email = (
                row.get("email")
                or row.get("account")
                or row.get("name")
                or ""
            )
            if isinstance(email, str) and "@" in email:
                found.add(email.strip().lower())
            for nest_key in ("auth", "data", "connection"):
                nest = row.get(nest_key)
                if isinstance(nest, dict):
                    e2 = nest.get("email") or nest.get("account") or ""
                    if isinstance(e2, str) and "@" in e2:
                        found.add(e2.strip().lower())
    return found


def filter_skip_existing(
    accounts: list[tuple[str, str]],
    already: set[str],
) -> tuple[list[tuple[str, str]], list[str]]:
    kept: list[tuple[str, str]] = []
    skipped: list[str] = []
    for email, password in accounts:
        if email.strip().lower() in already:
            skipped.append(email)
        else:
            kept.append((email, password))
    return kept, skipped


async def run_farm(
    accounts: list[tuple[str, str]],
    cfg: Config,
    prog: Progress,
    *,
    do_inject: bool = True,
    do_device_auth: bool = False,
    skip_exchange: bool = False,
    concurrency: int = 1,
    account_retries: int = 1,
    account_delay: float = 0.0,
    skip_existing: bool = False,
    skip_emails_file: Path | None = None,
) -> list[dict[str, Any]]:
    concurrency = max(1, int(concurrency))
    max_attempts = max(1, int(account_retries))
    delay_s = max(0.0, float(account_delay or 0.0))
    results: list[dict[str, Any]] = []
    sem = asyncio.Semaphore(concurrency)
    lock = asyncio.Lock()
    password_by_email = {e.lower(): p for e, p in accounts}

    work = list(accounts)
    if skip_existing:
        already = load_already_farmed_emails(
            output=cfg.output, skip_emails_file=skip_emails_file
        )
        work, skipped = filter_skip_existing(work, already)
        if skipped:
            prog.log(
                f"skip-existing: {len(skipped)} already farmed "
                f"(from output / skip list)",
                "INFO",
                step="skip",
            )
            for email in skipped[:20]:
                prog.log(f"skip {mask_email(email)}", "WAIT", email=mask_email(email))
            if len(skipped) > 20:
                prog.log(f"… +{len(skipped) - 20} more skipped", "INFO")
        prog.total = len(work)
        if not work:
            prog.log("nothing to farm after skip-existing", "INFO", step="skip")
            prog.summary()
            return []

    async def _one(email: str, password: str, index: int) -> None:
        async with sem:
            if delay_s > 0 and index > 0:
                stagger = delay_s * (index % max(concurrency, 1))
                if stagger > 0:
                    await asyncio.sleep(stagger)
            r: dict[str, Any] | None = None
            for attempt in range(1, max_attempts + 1):
                if attempt > 1:
                    label = mask_email(email)
                    prog.log(
                        f"account retry {attempt}/{max_attempts} after: "
                        f"{(r or {}).get('error') or 'fail'}",
                        "WARN",
                        email=label,
                    )
                    await asyncio.sleep(1.5 + 0.8 * (attempt - 1))
                is_last = attempt == max_attempts
                r = await process_one(
                    email,
                    password,
                    cfg,
                    prog,
                    do_inject=do_inject,
                    do_device_auth=do_device_auth,
                    skip_exchange=skip_exchange,
                    count_result=False,
                )
                if r.get("ok") and r.get("personalToken"):
                    prog.mark_ok(mask_email(email), "farmed")
                    break
                if is_last:
                    prog.mark_fail(
                        mask_email(email),
                        str(r.get("error") or "farm failed"),
                    )
            assert r is not None
            async with lock:
                results.append(r)
                if r.get("ok") and r.get("personalToken"):
                    try:
                        n, path = write_backup([r], cfg.output, append=True)
                        prog.log(f"saved -> {path} (+{n})", "INFO")
                        prog.account_ok(
                            email=email,
                            path=str(path),
                            masked_email=mask_email(email),
                        )
                    except Exception as exc:
                        prog.log(f"save err: {exc}", "ERR")
            if delay_s > 0 and concurrency == 1:
                await asyncio.sleep(delay_s)

    tasks = [
        asyncio.create_task(_one(e, p, i)) for i, (e, p) in enumerate(work)
    ]
    if tasks:
        await asyncio.gather(*tasks)

    failures = [r for r in results if not r.get("ok")]
    if failures:
        fail_path = cfg.output.with_name(cfg.output.stem + ".failures.json")
        failed_accounts_path = cfg.output.with_name("accounts.failed.txt")
        try:
            write_failures(failures, fail_path)
            prog.log(f"failures -> {fail_path}", "INFO")
        except Exception:
            pass
        try:
            n = write_failed_accounts(failures, password_by_email, failed_accounts_path)
            prog.log(
                f"failed accounts -> {failed_accounts_path} ({n} lines)",
                "INFO",
            )
        except Exception as exc:
            prog.log(f"failed accounts write err: {exc}", "ERR")

    prog.summary()
    return results
