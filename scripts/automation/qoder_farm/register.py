from __future__ import annotations

import asyncio
import traceback
from typing import Any

from .browser import close_session, launch_camoufox
from .config import Config
from .email_signup import generate_email, random_name, signup_with_email
from .export import write_backup
from .pat import approve_device_auth, create_pat, exchange_pat, fetch_quota
from .progress import Progress, mask_email
from .session import ensure_qoder_session


async def _screenshot(page: Any, cfg: Config, email: str, tag: str) -> None:
    try:
        cfg.screenshot_dir.mkdir(parents=True, exist_ok=True)
        safe = email.replace("@", "_at_").replace(".", "_")
        await page.screenshot(path=str(cfg.screenshot_dir / f"{safe}_{tag}.png"), full_page=True)
    except Exception:
        pass


async def register_one(
    email: str,
    password: str,
    cfg: Config,
    prog: Progress,
    *,
    do_device_auth: bool,
    skip_exchange: bool,
    first: str = "",
    last: str = "",
) -> dict[str, Any]:
    label = mask_email(email)
    result: dict[str, Any] = {
        "ok": False,
        "email": email,
        "password": password,
        "personalToken": None,
        "securityOauthToken": None,
        "authMethod": "email",
        "error": None,
    }
    session: dict[str, Any] | None = None
    try:
        prog.step(label, "browser", "launch camoufox")
        session = await launch_camoufox(cfg, prog)
        page = session["page"]

        if not await signup_with_email(page, email, password, cfg, prog, first, last):
            raise RuntimeError("email signup failed")

        prog.step(label, "session", "gate after signup")
        await ensure_qoder_session(page)

        pat = await create_pat(page, cfg, prog, label)
        result["personalToken"] = pat

        if do_device_auth:
            try:
                result["machineId"] = await approve_device_auth(page, cfg, prog, label)
            except Exception as exc:
                prog.log(f"device auth skipped: {exc}", "WAIT", email=label)

        if not skip_exchange:
            prog.step(label, "exchange", "PAT -> jobToken")
            try:
                exchanged = await exchange_pat(pat, cfg)
                result["securityOauthToken"] = exchanged.get("securityOauthToken") or ""
                result["refreshToken"] = exchanged.get("refreshToken") or ""
                raw = exchanged.get("raw") or {}
                if isinstance(raw, dict):
                    if raw.get("user_id") or raw.get("userId"):
                        result["userId"] = raw.get("user_id") or raw.get("userId")
                    if raw.get("expire_time") or raw.get("expireTime"):
                        result["expireTime"] = raw.get("expire_time") or raw.get("expireTime")
                prog.log("PAT exchange ok", "OK", email=label)
            except Exception as exc:
                prog.log(f"exchange failed (PAT still saved): {exc}", "WAIT", email=label)

        sot = result.get("securityOauthToken")
        if sot:
            try:
                result["quota"] = await fetch_quota(sot, cfg)
            except Exception as exc:
                prog.log(f"quota fetch err: {exc}", "DBG", email=label)

        result["ok"] = True
        return result
    except Exception as exc:
        result["error"] = str(exc)
        if cfg.debug:
            prog.log(traceback.format_exc(), "DBG", email=label)
        if session and session.get("page"):
            await _screenshot(session["page"], cfg, email, "error")
        return result
    finally:
        await close_session(session)


async def run_register(
    accounts: list[tuple[str, str]],
    cfg: Config,
    prog: Progress,
    *,
    count: int = 0,
    do_device_auth: bool = False,
    skip_exchange: bool = False,
    concurrency: int = 1,
    account_retries: int = 2,
    account_delay: float = 0.0,
) -> list[dict[str, Any]]:
    if count > 0 and cfg.email_source:
        gen = []
        for _ in range(count):
            first, last = random_name()
            gen.append((generate_email(cfg.email_source, first, last), cfg.register_password, first, last))
        accounts = [(e, p) for e, p, _, _ in gen]
        names = {e: (f, l) for e, p, f, l in gen}
    else:
        names = {}
    if not accounts:
        prog.log("no accounts to register (need count+email_source or -f)", "ERR", step="start")
        return []

    prog.total = len(accounts)
    concurrency = max(1, int(concurrency))
    max_attempts = max(1, int(account_retries))
    delay_s = max(0.0, float(account_delay or 0.0))
    results: list[dict[str, Any]] = []
    sem = asyncio.Semaphore(concurrency)
    lock = asyncio.Lock()

    async def _one(email: str, password: str, index: int) -> None:
        async with sem:
            if delay_s > 0 and index > 0 and concurrency > 1:
                await asyncio.sleep(delay_s * (index % concurrency))
            r: dict[str, Any] | None = None
            for attempt in range(1, max_attempts + 1):
                if attempt > 1:
                    prog.log(
                        f"account retry {attempt}/{max_attempts}: {(r or {}).get('error') or 'fail'}",
                        "WARN",
                        email=mask_email(email),
                    )
                    await asyncio.sleep(1.5 + 0.8 * (attempt - 1))
                r = await register_one(
                    email,
                    password,
                    cfg,
                    prog,
                    do_device_auth=do_device_auth,
                    skip_exchange=skip_exchange,
                    first=names.get(email, ("", ""))[0],
                    last=names.get(email, ("", ""))[1],
                )
                if r.get("ok") and r.get("personalToken"):
                    prog.mark_ok(mask_email(email), "registered + PAT")
                    break
                if attempt == max_attempts:
                    prog.mark_fail(mask_email(email), str(r.get("error") or "register failed"))
            assert r is not None
            async with lock:
                results.append(r)
                if r.get("ok") and r.get("personalToken"):
                    try:
                        n, path = write_backup([r], cfg.output, append=True)
                        prog.log(f"saved -> {path} (+{n})", "INFO")
                        prog.account_ok(email=email, path=str(path), masked_email=mask_email(email))
                    except Exception as exc:
                        prog.log(f"save err: {exc}", "ERR")
            if delay_s > 0 and concurrency == 1:
                await asyncio.sleep(delay_s)

    tasks = [asyncio.create_task(_one(e, p, i)) for i, (e, p) in enumerate(accounts)]
    if tasks:
        await asyncio.gather(*tasks)

    prog.summary()
    return results
