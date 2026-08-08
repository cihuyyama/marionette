"""GSuite / Google SSO runner for blackbox_farm.

Second mode alongside register.py: input is existing Google accounts
(email|password lines) instead of freshly-created temp-mail accounts. No
mailbox, no OTP — the Google password is the only credential. Orchestration
mirrors register.py (semaphore workers, per-account wall-clock budget,
retries, abort-after-4-consecutive-failures, incremental backup write +
NDJSON account_ok events for Marionette auto-import).
"""
from __future__ import annotations

import asyncio
from typing import Any

from .browser_flow import BlackboxClient, BlackboxError
from .config import Config
from .export import write_backup
from .progress import Progress, mask_email
from .validate import ValidationError, validate_key


_STEP_NAMES = {
    "opening login...": "login",
    "google sign-in...": "sso",
    "waiting for app...": "wait_app",
    "creating api key...": "create_key",
    "done": "done",
}


async def run_google_sso(
    cfg: Config,
    prog: Progress,
    accounts: list[tuple[str, str]],
    concurrency: int = 1,
    account_retries: int = 1,
    account_delay: float = 0.0,
) -> list[dict]:
    concurrency = max(1, int(concurrency))
    max_attempts = max(1, int(account_retries))
    delay_s = max(0.0, float(account_delay or 0.0))
    budget = max(120.0, float(cfg.account_timeout))

    results: list[dict] = []
    sem = asyncio.Semaphore(concurrency)
    save_lock = asyncio.Lock()
    stop_flag = {"stop": False}
    failed_streak = {"n": 0}
    started_emails: set[str] = set()
    settled_emails: set[str] = set()

    def _settle(email: str, ok: bool, msg: str) -> None:
        settled_emails.add(email)
        if ok:
            failed_streak["n"] = 0
            prog.mark_ok(email, msg)
        else:
            failed_streak["n"] += 1
            prog.mark_fail(email, msg)

    async def _attempt_one(email: str, password: str) -> dict:
        loop = asyncio.get_event_loop()
        client: BlackboxClient | None = None
        try:
            started_emails.add(email)

            def on_step(msg: str) -> None:
                prog.step(email, _STEP_NAMES.get(msg, "flow"), msg)

            def log_fn(msg: str, level: str) -> None:
                prog.log(msg, level, email=email, step="flow")

            prog.step(email, "launch", "starting camoufox")
            client = BlackboxClient(cfg, log_fn=log_fn)
            await client.start()

            prog.step(email, "sso", "google sign-in")
            api_key = await client.login_with_google_and_create_key(
                email, password, on_step
            )

            prog.step(email, "validate", "probing key against api.blackbox.ai")
            try:
                await loop.run_in_executor(None, validate_key, api_key)
            except ValidationError as exc:
                raise BlackboxError(str(exc)) from exc
            prog.log(f"key valid: {api_key[:12]}...", "DBG", email=email, step="validate")

            return {
                "ok": True,
                "email": email,
                "password": password,
                "apiKey": api_key,
            }
        finally:
            if client is not None:
                try:
                    await client.stop()
                except Exception:
                    pass

    async def _run_worker(idx: int, email: str, password: str) -> None:
        async with sem:
            if stop_flag["stop"]:
                prog.log(
                    f"skip {mask_email(email)} — abort after consecutive failures",
                    "WARN",
                    step="stop",
                )
                return
            if delay_s > 0 and idx > 0:
                stagger = delay_s * (idx % max(concurrency, 1))
                if stagger > 0:
                    await asyncio.sleep(stagger)

            last_err = ""
            row: dict[str, Any] = {}
            succeeded = False
            for attempt in range(1, max_attempts + 1):
                if attempt > 1:
                    prog.log(
                        f"account retry {attempt}/{max_attempts} after: {last_err}",
                        "WARN",
                        email=email,
                    )
                    await asyncio.sleep(1.5 + 0.8 * (attempt - 1))
                try:
                    row = await asyncio.wait_for(
                        _attempt_one(email, password), timeout=budget
                    )
                    succeeded = True
                    break
                except asyncio.TimeoutError:
                    last_err = f"timeout after {budget:.0f}s budget"
                except asyncio.CancelledError:
                    try:
                        for em in started_emails - settled_emails:
                            _settle(em, False, "cancelled (job budget exceeded)")
                    except Exception:
                        pass
                    raise
                except Exception as exc:
                    err = str(exc)
                    if "Timeout" in err or "timeout" in err:
                        last_err = "timeout during google sso (check proxy/network)"
                    else:
                        last_err = err[:150]

            if succeeded:
                async with save_lock:
                    results.append(row)
                    try:
                        n, path = write_backup([row], cfg.output, append=True)
                        prog.log(f"saved -> {path} (+{n})", "INFO", email=email)
                        prog.account_ok(
                            email=email,
                            path=str(path),
                            masked_email=mask_email(email),
                        )
                    except Exception as exc:
                        prog.log(f"save err: {exc}", "ERR", email=email)
                _settle(email, True, "sso login + api key harvested")
            else:
                _settle(email, False, last_err or "no result")

            if delay_s > 0 and concurrency == 1:
                await asyncio.sleep(delay_s)

        # Same abort guard as register mode: if the whole batch keeps failing
        # (e.g. farm IP blocked by Google or Blackbox), stop burning browsers.
        if failed_streak["n"] >= 4 and not stop_flag["stop"]:
            stop_flag["stop"] = True
            prog.log(
                "aborting remaining accounts after 4 consecutive failures",
                "ERR",
                step="stop",
            )

    tasks = [
        asyncio.create_task(_run_worker(i, e, p)) for i, (e, p) in enumerate(accounts)
    ]
    await asyncio.gather(*tasks, return_exceptions=True)
    for email in sorted(started_emails - settled_emails):
        prog.mark_fail(email, "no terminal state recorded (driver crash?)")
    prog.summary()
    return results
