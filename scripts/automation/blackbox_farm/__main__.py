from __future__ import annotations

import argparse
import asyncio
import sys
from dataclasses import replace
from pathlib import Path

from .config import load_config
from .progress import Progress
from .register import run_register


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="python -m blackbox_farm",
        description=(
            "Blackbox.ai account farm. register mode: signup new accounts on "
            "app.blackbox.ai (Playwright Chromium) -> OTP via self-hosted "
            "cloudflare temp-mail worker -> create + harvest sk-... API key."
        ),
    )
    p.add_argument(
        "-f",
        "--file",
        dest="file",
        help="Accounts file. Register mode: single line 'register:COUNT:domain'",
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

    # Register mode is the only mode: auto-detect from accounts text.
    raw_text = ""
    if args.file:
        fp = Path(args.file)
        if fp.is_file():
            raw_text = fp.read_text(encoding="utf-8").strip()
    directive = _parse_register_directive(raw_text)
    if directive is None:
        print(
            "No register directive. Put one line in -f accounts.txt:\n"
            "  register:COUNT:domain\n"
            "See accounts.txt.example",
            file=sys.stderr,
        )
        return 2

    count, _domain = directive  # domain reserved; mailbox domain comes from CF worker
    concurrency = max(1, int(args.concurrency or 1))
    account_retries = max(1, int(args.account_retries or 1))
    account_delay = max(0.0, float(args.account_delay or 0.0))

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


if __name__ == "__main__":
    raise SystemExit(main())
