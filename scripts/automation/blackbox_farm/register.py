from __future__ import annotations

import asyncio
import secrets
from typing import Any

from .browser_flow import BlackboxClient, BlackboxError
from .config import Config
from .export import write_backup
from .mail import TempMailSession, cf_mail_configured, create_cf_client, extract_otp
from .progress import Progress, mask_email
from .validate import ValidationError, validate_key


def generate_password() -> str:
    """Novabox-format strong password: N + hex + fixed spice + urlsafe tail."""
    return "N" + secrets.token_hex(4) + "!a7#" + secrets.token_urlsafe(6)


_STEP_NAMES = {
    "signing up...": "signup",
    "waiting for otp...": "wait_otp",
    "verifying otp...": "verify_otp",
    "creating api key...": "create_key",
    "done": "done",
}


async def run_register(
    cfg: Config,
    prog: Progress,
    count: int = 1,
    concurrency: int = 1,
    account_retries: int = 1,
    account_delay: float = 0.0,
) -> list[dict]:
    """Concurrent semaphore worker: signup + OTP + key harvest per account.

    Mirrors grok_farm/register.py orchestration: semaphore, per-account
    wall-clock budget (asyncio.wait_for), started/settled email guards with
    reconcile, abort after 4 consecutive failures, prog.summary() at end.
    account_delay keeps grok_farm semantics: stagger between workers when
    concurrent, inter-account sleep when serial — only if > 0.
    """
    register_label = f"register:{count}"
    if not cf_mail_configured(cfg):
        prog.mark_fail(
            register_label,
            "temp-mail worker not configured (BLACKBOX_CF_MAIL_BASE_URL / "
            "ADMIN_PASSWORD / DOMAIN) — register mode needs it",
        )
        prog.summary()
        return []

    prog.log(
        f"signup mail via temp-mail worker ({cfg.cf_mail_domain})", "INFO", step="start"
    )

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

    async def _attempt_one(idx: int) -> dict:
        """One full pipeline attempt: mailbox -> browser -> key -> validate."""
        loop = asyncio.get_event_loop()

        cf_client = create_cf_client(cfg)
        addr, jwt, addr_id = await loop.run_in_executor(None, cf_client.create_address)
        email = addr
        mail_session = TempMailSession(
            email=addr,
            jwt=jwt,
            address_id=addr_id,
            client=cf_client,
            extract_otp=extract_otp,
        )
        prog.log(f"temp-mail {addr}", "INFO", step="start")

        client: BlackboxClient | None = None
        try:
            acc_password = generate_password()
            started_emails.add(email)

            async def wait_otp(_email: str) -> str:
                code = await loop.run_in_executor(
                    None, mail_session.poll_otp, cfg.otp_timeout, cfg.otp_poll_interval
                )
                if code:
                    prog.log(f"OTP received: {code}", "DBG", email=email, step="wait_otp")
                return code or ""

            def on_step(msg: str) -> None:
                prog.step(email, _STEP_NAMES.get(msg, "flow"), msg)

            prog.step(email, "launch", "starting chromium")
            client = BlackboxClient(cfg)
            await client.start()

            prog.step(email, "signup", "filling signup form")
            api_key = await client.register_and_create_key(
                email, acc_password, wait_otp, on_step
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
                "password": acc_password,
                "apiKey": api_key,
            }
        finally:
            if client is not None:
                try:
                    await client.stop()
                except Exception:
                    pass
            try:
                await loop.run_in_executor(None, mail_session.cleanup)
            except Exception:
                pass

    async def _run_worker(idx: int) -> None:
        async with sem:
            if stop_flag["stop"]:
                prog.log(
                    f"skip worker {idx} — abort after consecutive failures",
                    "WARN",
                    step="stop",
                )
                return
            if delay_s > 0 and idx > 0:
                stagger = delay_s * (idx % max(concurrency, 1))
                if stagger > 0:
                    await asyncio.sleep(stagger)

            last_err = ""
            final_email = ""
            succeeded = False
            for attempt in range(1, max_attempts + 1):
                if attempt > 1:
                    prog.log(
                        f"account retry {attempt}/{max_attempts} after: {last_err}",
                        "WARN",
                        email=final_email,
                    )
                    await asyncio.sleep(1.5 + 0.8 * (attempt - 1))
                try:
                    row = await asyncio.wait_for(_attempt_one(idx), timeout=budget)
                    final_email = row["email"]
                    succeeded = True
                    break
                except asyncio.TimeoutError:
                    last_err = f"timeout after {budget:.0f}s budget"
                    final_email = _settle_attempt_emails(started_emails, settled_emails, prog) or final_email
                except asyncio.CancelledError:
                    # Only the job teardown path cancels us; record a terminal
                    # state before re-raising so no account silently vanishes.
                    try:
                        for em in started_emails - settled_emails:
                            _settle(em, False, "cancelled (job budget exceeded)")
                    except Exception:
                        pass
                    raise
                except Exception as exc:
                    err = str(exc)
                    if "Timeout" in err or "timeout" in err:
                        last_err = "timeout during registration (check proxy/network)"
                    else:
                        last_err = err[:150]
                    final_email = _settle_attempt_emails(started_emails, settled_emails, prog) or final_email

            if succeeded:
                row_for_export = {**row, "ok": True}  # type: ignore[possibly-undefined]
                async with save_lock:
                    results.append(row_for_export)
                    try:
                        n, path = write_backup([row_for_export], cfg.output, append=True)
                        prog.log(f"saved -> {path} (+{n})", "INFO", email=final_email)
                        prog.account_ok(
                            email=final_email,
                            path=str(path),
                            masked_email=mask_email(final_email),
                        )
                    except Exception as exc:
                        prog.log(f"save err: {exc}", "ERR", email=final_email)
                _settle(final_email, True, "registered + api key harvested")
            else:
                _settle(final_email or f"worker-{idx}", False, last_err or "no result")

            if delay_s > 0 and concurrency == 1:
                await asyncio.sleep(delay_s)

        # Abort the remaining backlog when the whole batch keeps failing
        # (e.g. Blackbox blocking the farm IP): burning browsers per account
        # would waste capacity with the same outcome.
        if failed_streak["n"] >= 4 and not stop_flag["stop"]:
            stop_flag["stop"] = True
            prog.log(
                "aborting remaining accounts after 4 consecutive failures",
                "ERR",
                step="stop",
            )

    tasks = [asyncio.create_task(_run_worker(i)) for i in range(count)]
    await asyncio.gather(*tasks, return_exceptions=True)
    # Settled guard: a driver-level crash can kill every worker mid-await
    # before any terminal line is emitted — reconcile here so no account
    # silently vanishes from the log.
    for email in sorted(started_emails - settled_emails):
        prog.mark_fail(email, "no terminal state recorded (driver crash?)")
    prog.summary()
    return results


def _settle_attempt_emails(
    started: set[str], settled: set[str], prog: Progress
) -> str | None:
    """Mark leftover attempt emails as processed (without counting fails).

    A failed attempt's mailbox email must not be re-reported by the final
    reconcile guard; only the worker's final outcome touches ok/fail counts.
    Returns the last email settled (for fail attribution).
    """
    last: str | None = None
    for email in sorted(started - settled):
        settled.add(email)
        prog.log(f"attempt failed for {mask_email(email)}", "DBG", email=email)
        last = email
    return last
