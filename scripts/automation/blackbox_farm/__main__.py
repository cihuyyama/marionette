from __future__ import annotations

import argparse
import asyncio
import sys
from dataclasses import replace
from pathlib import Path

from .config import load_config
from .progress import Progress
from .register import run_register
from .sso import run_google_sso


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="python -m blackbox_farm",
        description=(
            "Blackbox.ai account farm. Two modes: (1) register — signup new "
            "accounts (temp-mail OTP) then harvest sk-... API key; "
            "(2) google-sso — login existing GSuite/Google accounts via "
            "'Continue with Google' (email|password lines) and harvest the key."
        ),
    )
    p.add_argument(
        "-f",
        "--file",
        dest="file",
        help=(
            "Accounts file. Register mode: single line 'register:COUNT:domain'. "
            "Google-SSO mode: one 'email|password' per line."
        ),
    )
    p.add_argument(
        "-o",
        "--output",
        help="Output JSON path (default: BLACKBOX_OUTPUT / results/blackbox-accounts.json)",
    )
    p.add_argument(
        "--concurrency",
        type=int,
        default=1,
        help="Parallel browsers (default 1)",
    )
    p.add_argument(
        "--headless",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Override BLACKBOX_HEADLESS",
    )
    p.add_argument(
        "--account-retries",
        type=int,
        default=1,
        help="Attempts per account for full pipeline (default 1)",
    )
    p.add_argument(
        "--account-delay",
        type=float,
        default=0.0,
        help="Seconds between accounts (serial) or stagger between workers",
    )
    p.add_argument(
        "--json-progress",
        action="store_true",
        help="Emit NDJSON progress events on stdout (for Marionette dashboard)",
    )
    p.add_argument(
        "--debug",
        action="store_true",
        help="Verbose debug logs + screenshots on error",
    )
    # Runner compatibility (src/farm.rs passes these to every farm package).
    # novabox's flow uses plain chromium without proxies and register mode
    # always creates fresh addresses, so the flags are accepted, not applied.
    p.add_argument("--proxy-file", default=None, help=argparse.SUPPRESS)
    p.add_argument(
        "--no-proxy", action="store_true", help=argparse.SUPPRESS
    )
    p.add_argument(
        "--skip-existing", action="store_true", help=argparse.SUPPRESS
    )
    p.add_argument("--skip-emails-file", default=None, help=argparse.SUPPRESS)
    return p


def _parse_register_directive(text: str) -> tuple[int, str] | None:
    """'register:COUNT:domain' -> (count, domain)."""
    parts = text.strip().split(":", 2)
    if len(parts) < 2 or parts[0] != "register":
        return None
    count = int(parts[1]) if parts[1].isdigit() else 1
    domain = parts[2].strip() if len(parts) > 2 else ""
    return max(1, count), domain


def parse_sso_accounts(raw_text: str) -> list[tuple[str, str]]:
    """Parse email|password lines (same shapes as farm.rs parse_accounts_text)."""
    out: list[tuple[str, str]] = []
    seen: set[str] = set()
    for line in raw_text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "|" in line:
            email, _, password = line.partition("|")
        else:
            at = line.find("@")
            if at < 0:
                continue
            colon = line.find(":", at)
            if colon < 0:
                continue
            email, password = line[:colon], line[colon + 1 :]
        email = email.strip()
        password = password.strip()
        if not email or not password or "@" not in email:
            continue
        key = email.lower()
        if key in seen:
            continue
        seen.add(key)
        out.append((email, password))
    return out


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    cfg = load_config()

    overrides: dict = {}
    if args.output:
        out = Path(args.output)
        if not out.is_absolute():
            out = cfg.root / out
        overrides["output"] = out
    if args.headless is not None:
        overrides["headless"] = args.headless
    if args.debug:
        overrides["debug"] = True
    if args.json_progress:
        overrides["json_progress"] = True
    if overrides:
        cfg = replace(cfg, **overrides)

    raw_text = ""
    if args.file:
        fp = Path(args.file)
        if fp.is_file():
            raw_text = fp.read_text(encoding="utf-8").strip()

    directive = _parse_register_directive(raw_text)
    concurrency = max(1, int(args.concurrency or 1))
    account_retries = max(1, int(args.account_retries or 1))
    account_delay = max(0.0, float(args.account_delay or 0.0))

    if directive is not None:
        count, _domain = directive  # domain reserved; mailbox domain comes from CF worker
        prog = Progress(
            ui="log",
            debug=cfg.debug,
            json_progress=cfg.json_progress,
            total=count,
        )
        prog.log(
            f"mode=register count={count} concurrency={concurrency} "
            f"headless={cfg.headless} account_retries={account_retries} "
            f"account_delay={account_delay} out={cfg.output}",
            "INFO",
            step="start",
        )
        results = asyncio.run(
            run_register(
                cfg,
                prog,
                count=count,
                concurrency=concurrency,
                account_retries=account_retries,
                account_delay=account_delay,
            )
        )
        failed = count - len(results)
        return 0 if failed == 0 else 1

    accounts = parse_sso_accounts(raw_text)
    if not accounts:
        print(
            "No accounts found. Put one of these in -f accounts.txt:\n"
            "  register:COUNT:domain            (signup new accounts via temp-mail OTP)\n"
            "  email|password  (one per line)   (Google SSO login, no OTP)\n"
            "See accounts.txt.example",
            file=sys.stderr,
        )
        return 2

    prog = Progress(
        ui="log",
        debug=cfg.debug,
        json_progress=cfg.json_progress,
        total=len(accounts),
    )
    prog.log(
        f"mode=google-sso accounts={len(accounts)} concurrency={concurrency} "
        f"headless={cfg.headless} account_retries={account_retries} "
        f"account_delay={account_delay} out={cfg.output}",
        "INFO",
        step="start",
    )
    results = asyncio.run(
        run_google_sso(
            cfg,
            prog,
            accounts,
            concurrency=concurrency,
            account_retries=account_retries,
            account_delay=account_delay,
        )
    )
    failed = len(accounts) - len(results)
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
