from __future__ import annotations

import asyncio
import traceback
from pathlib import Path
from typing import Any

from .browser import close_session, launch_camoufox, save_cookies
from .config import Config
from .export import emails_in_output, write_backup, write_failures
from .login import do_email_login
from .oauth import obtain_oidc_tokens
from .activate import activate_grok_if_needed
from .progress import Progress, mask_email
from .verify import verify_chat


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


def load_skip_emails(
    *,
    output: Path,
    skip_emails_file: Path | None = None,
) -> set[str]:
    found = emails_in_output(output)
    if skip_emails_file and skip_emails_file.is_file():
        try:
            for line in skip_emails_file.read_text(
                encoding="utf-8", errors="replace"
            ).splitlines():
                raw = line.strip()
                if not raw or raw.startswith("#"):
                    continue
                if raw.startswith("{"):
                    try:
                        import json

                        row = json.loads(raw)
                        email = str(row.get("email") or "").strip().lower()
                        if email and "@" in email:
                            found.add(email)
                        continue
                    except Exception:
                        pass
                email = raw.split("|", 1)[0].split(":", 1)[0].strip().lower()
                if email and "@" in email:
                    found.add(email)
        except OSError:
            pass
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


async def process_one(
    email: str,
    password: str,
    cfg: Config,
    prog: Progress,
    *,
    skip_verify: bool = False,
    count_result: bool = True,
) -> dict[str, Any]:
    """
    browser -> login -> oauth PKCE -> verify_chat -> result dict.
    """
    label = mask_email(email)
    result: dict[str, Any] = {
        "ok": False,
        "email": email,
        "accessToken": None,
        "refreshToken": None,
        "idToken": None,
        "clientId": cfg.client_id,
        "expiresAt": None,
        "expiresIn": None,
        "scope": None,
        "verified": False,
        "error": None,
    }
    session: dict[str, Any] | None = None

    try:
        prog.step(label, "browser", "launch camoufox")
        session = await launch_camoufox(cfg, prog, email=email)
        page = session["page"]

        # Optional warm login on accounts.x.ai (OAuth path also drives login)
        try:
            await do_email_login(page, email, password, cfg, prog, label)
        except Exception as exc:
            prog.log(f"pre-login note: {exc}", "WAIT", email=label)

        # Ensure account has Grok entitlement (principalId) before OAuth
        await activate_grok_if_needed(page, cfg, prog, label)

        tokens = await obtain_oidc_tokens(page, email, password, cfg, prog, label)
        access = tokens.get("access_token") or ""
        refresh = tokens.get("refresh_token") or ""
        if not access or not refresh:
            raise RuntimeError("OAuth returned incomplete tokens")

        result["accessToken"] = access
        result["refreshToken"] = refresh
        result["idToken"] = tokens.get("id_token") or ""
        result["clientId"] = tokens.get("client_id") or cfg.client_id
        result["expiresAt"] = tokens.get("expires_at")
        result["expiresIn"] = tokens.get("expires_in")
        result["scope"] = tokens.get("scope") or cfg.scope
        result["auth_mode"] = tokens.get("auth_mode") or "oidc"
        if tokens.get("email"):
            result["email"] = tokens["email"]

        if skip_verify or cfg.skip_verify:
            prog.log("verify_chat skipped (--skip-verify)", "WAIT", email=label)
            result["verified"] = False
        else:
            prog.step(label, "verify", "chat ACTIVE probe")
            # run blocking urllib off the event loop
            loop = asyncio.get_event_loop()
            await loop.run_in_executor(None, lambda: verify_chat(access, cfg))
            result["verified"] = True
            prog.log("verify_chat ACTIVE ok", "OK", email=label)

        result["ok"] = True
        await save_cookies(page, email)
        if count_result:
            prog.mark_ok(label, "relogin ok")
        else:
            prog.log("relogin ok", "OK", email=label)
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


async def run_relogin(
    accounts: list[tuple[str, str]],
    cfg: Config,
    prog: Progress,
    *,
    concurrency: int = 1,
    account_retries: int = 1,
    account_delay: float = 0.0,
    skip_existing: bool = False,
    skip_emails_file: Path | None = None,
    skip_verify: bool = False,
) -> list[dict[str, Any]]:
    """
    Concurrent semaphore worker: process_one with retries, incremental JSON write,
    account_ok NDJSON on success.
    """
    concurrency = max(1, int(concurrency))
    max_attempts = max(1, int(account_retries))
    delay_s = max(0.0, float(account_delay or 0.0))
    results: list[dict[str, Any]] = []
    sem = asyncio.Semaphore(concurrency)
    lock = asyncio.Lock()
    password_by_email = {e.lower(): p for e, p in accounts}

    work = list(accounts)
    if skip_existing:
        already = load_skip_emails(output=cfg.output, skip_emails_file=skip_emails_file)
        work, skipped = filter_skip_existing(work, already)
        if skipped:
            prog.log(
                f"skip-existing: {len(skipped)} already in output / skip list",
                "INFO",
                step="skip",
            )
            for email in skipped[:20]:
                prog.log(f"skip {mask_email(email)}", "WAIT", email=mask_email(email))
            if len(skipped) > 20:
                prog.log(f"… +{len(skipped) - 20} more skipped", "INFO")
        prog.total = len(work)
        if not work:
            prog.log("nothing to relogin after skip-existing", "INFO", step="skip")
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
                    skip_verify=skip_verify,
                    count_result=False,
                )
                if r.get("ok") and r.get("accessToken"):
                    prog.mark_ok(mask_email(email), "relogin ok")
                    break
                if is_last:
                    prog.mark_fail(
                        mask_email(email),
                        str(r.get("error") or "relogin failed"),
                    )
            assert r is not None
            async with lock:
                results.append(r)
                if r.get("ok") and r.get("accessToken"):
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

    tasks = [asyncio.create_task(_one(e, p, i)) for i, (e, p) in enumerate(work)]
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
            prog.log(f"failed accounts -> {failed_accounts_path} ({n} lines)", "INFO")
        except Exception as exc:
            prog.log(f"failed accounts write err: {exc}", "ERR")

    prog.summary()
    return results
