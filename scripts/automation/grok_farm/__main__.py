from __future__ import annotations

import argparse
import asyncio
import sys
from dataclasses import replace
from pathlib import Path

from .config import load_config
from .export import write_backup
from .progress import Progress, load_accounts
from .relogin import run_relogin


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="python -m grok_farm",
        description=(
            "Grok CLI manual thin/mass relogin "
            "(email+password -> OAuth PKCE -> verify_chat -> marionette-import JSON). "
            "No signup farm. No 9Router DB writes."
        ),
    )
    p.add_argument(
        "accounts",
        nargs="*",
        help="Inline email|password (or email:password)",
    )
    p.add_argument(
        "-f",
        "--file",
        dest="file",
        help="Accounts file (email|password or JSONL per line)",
    )
    p.add_argument(
        "-o",
        "--output",
        help="Output JSON path (default: GROK_OUTPUT / results/grok-accounts.json)",
    )
    p.add_argument(
        "--concurrency",
        type=int,
        default=1,
        help="Parallel browsers (default 1)",
    )
    p.add_argument(
        "--account-retries",
        type=int,
        default=2,
        help="Attempts per account for full pipeline (default 2)",
    )
    p.add_argument(
        "--account-delay",
        type=float,
        default=0.0,
        help="Seconds between accounts (serial) or stagger between workers",
    )
    p.add_argument(
        "--skip-existing",
        action="store_true",
        help="Skip emails already in -o output JSON (or --skip-emails-file)",
    )
    p.add_argument(
        "--skip-emails-file",
        default=None,
        help="Extra email list (one per line or email|password) to skip",
    )
    p.add_argument(
        "--skip-verify",
        action="store_true",
        help="Skip verify_chat ACTIVE probe (debug only; default off)",
    )
    p.add_argument(
        "--proxy-file",
        default=None,
        help="Proxy list file (one URL per line; rotates per browser launch)",
    )
    p.add_argument(
        "--headless",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Override GROK_HEADLESS",
    )
    p.add_argument(
        "--ui",
        choices=("log", "hud"),
        default=None,
        help="Progress UI (default: env GROK_UI=log)",
    )
    p.add_argument(
        "--debug",
        action="store_true",
        help="Verbose debug logs + screenshots on error",
    )
    p.add_argument(
        "--json-progress",
        action="store_true",
        help="Emit NDJSON progress events on stdout (for Marionette dashboard)",
    )
    return p


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
    if args.ui is not None:
        overrides["ui"] = args.ui
    if args.debug:
        overrides["debug"] = True
    if args.json_progress:
        overrides["json_progress"] = True
    if args.proxy_file:
        overrides["proxy_file"] = str(args.proxy_file)
    if args.skip_verify:
        overrides["skip_verify"] = True
    if overrides:
        cfg = replace(cfg, **overrides)

    accounts = load_accounts(args.file, list(args.accounts or []))
    if not accounts:
        print(
            "No accounts. Pass email|password args or -f accounts.txt\n"
            "See accounts.txt.example",
            file=sys.stderr,
        )
        return 2

    skip_emails_path: Path | None = None
    if args.skip_emails_file:
        skip_emails_path = Path(args.skip_emails_file)
        if not skip_emails_path.is_absolute():
            skip_emails_path = cfg.root / skip_emails_path

    prog = Progress(
        ui=cfg.ui,
        debug=cfg.debug,
        json_progress=cfg.json_progress,
        total=len(accounts),
    )
    account_retries = max(1, int(args.account_retries or 1))
    account_delay = max(0.0, float(args.account_delay or 0.0))
    prog.log(
        f"accounts={len(accounts)} headless={cfg.headless} "
        f"humanize={cfg.humanize} concurrency={args.concurrency} "
        f"account_retries={account_retries} account_delay={account_delay} "
        f"skip_existing={bool(args.skip_existing)} "
        f"skip_verify={bool(args.skip_verify or cfg.skip_verify)} "
        f"out={cfg.output}",
        "INFO",
        step="start",
    )

    results = asyncio.run(
        run_relogin(
            accounts,
            cfg,
            prog,
            concurrency=max(1, int(args.concurrency)),
            account_retries=account_retries,
            account_delay=account_delay,
            skip_existing=bool(args.skip_existing),
            skip_emails_file=skip_emails_path,
            skip_verify=bool(args.skip_verify or cfg.skip_verify),
        )
    )

    ok = [r for r in results if r.get("ok") and r.get("accessToken")]
    if ok:
        n, path = write_backup(ok, cfg.output, append=True)
        print(
            f"\nImport with:\n  just import-json {path}\n  # or\n"
            f"  cargo run --bin marionette-import -- --file {path}"
        )
        print(f"Wrote/updated {n} connection(s) -> {path}")

    failed = sum(1 for r in results if not r.get("ok"))
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
