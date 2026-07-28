from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path

from .config import load_config
from .export import write_backup
from .farm import run_farm
from .progress import Progress, load_accounts



def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="python -m qoder_farm",
        description=(
            "Qoder farm (GSuite SSO -> PAT -> optional dudul inject) "
            "-> marionette-import JSON. "
            "Also: --inject-only --pat pt-… for per-account dudul inject "
            "(Accounts UI / admin API)."
        ),
    )
    p.add_argument(
        "--inject-only",
        action="store_true",
        help=(
            "Skip farm SSO; only run dudul inject for --pat "
            "(activate Pro Trial on existing account)"
        ),
    )
    p.add_argument(
        "--pat",
        default=None,
        help="PAT for --inject-only mode (pt-…)",
    )
    p.add_argument(
        "--email",
        default="",
        help="Email label for --inject-only logs",
    )
    p.add_argument(
        "--label",
        default="",
        help="Extra label for --inject-only",
    )
    p.add_argument(
        "--json-result",
        action="store_true",
        default=True,
        help=argparse.SUPPRESS,
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
        help="Accounts file (email|password per line)",
    )
    p.add_argument(
        "-o",
        "--output",
        help="Output JSON path (default: QODER_OUTPUT / results/qoder-accounts.json)",
    )
    p.add_argument(
        "--inject",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Enable dudul inject (default: env QODER_DUDUL_INJECT)",
    )
    p.add_argument(
        "--device-auth",
        action="store_true",
        help="Also hit device Continue (trial trigger)",
    )
    p.add_argument(
        "--skip-exchange",
        action="store_true",
        help="Skip openapi PAT→jobToken exchange (still save PAT)",
    )
    p.add_argument(
        "--concurrency",
        type=int,
        default=1,
        help="Parallel browsers (default 1; keep 1 for Turnstile)",
    )
    p.add_argument(
        "--account-retries",
        type=int,
        default=2,
        help=(
            "Attempts per account for full pipeline (SSO→PAT→…). "
            "Default 2. Failed emails written to accounts.failed.txt"
        ),
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
        "--proxy-file",
        default=None,
        help="Proxy list file (one URL per line; rotates per browser launch)",
    )
    p.add_argument(
        "--headless",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Override QODER_HEADLESS",
    )
    p.add_argument(
        "--ui",
        choices=("log", "hud"),
        default=None,
        help="Progress UI (default: env QODER_UI=log)",
    )
    p.add_argument(
        "--debug",
        action="store_true",
        help="Verbose debug logs + screenshots on error",
    )
    p.add_argument(
        "--settle",
        type=int,
        default=None,
        help="Seconds to wait after PAT before inject (default env)",
    )
    p.add_argument(
        "--json-progress",
        action="store_true",
        help="Emit NDJSON progress events on stdout (for Marionette dashboard)",
    )
    return p


def main(argv: list[str] | None = None) -> int:
    raw = list(sys.argv[1:] if argv is None else argv)
    if "--inject-only" in raw:
        from .inject_only import main_inject_only

        rest = [a for a in raw if a != "--inject-only"]
        return main_inject_only(rest)

    args = build_parser().parse_args(argv)
    cfg = load_config()

    # CLI overrides via object replace (frozen dataclass → rebuild)
    from dataclasses import replace

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
    if args.settle is not None:
        overrides["inject_settle_secs"] = max(0, args.settle)
    if args.inject is not None:
        overrides["dudul_inject"] = args.inject
    if args.json_progress:
        overrides["json_progress"] = True
    if args.proxy_file:
        overrides["proxy_file"] = str(args.proxy_file)
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

    do_inject = cfg.dudul_inject if args.inject is None else bool(args.inject)

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
        f"accounts={len(accounts)} inject={do_inject} "
        f"headless={cfg.headless} concurrency={args.concurrency} "
        f"account_retries={account_retries} account_delay={account_delay} "
        f"skip_existing={bool(args.skip_existing)} out={cfg.output}",
        "INFO",
        step="start",
    )

    results = asyncio.run(
        run_farm(
            accounts,
            cfg,
            prog,
            do_inject=do_inject,
            do_device_auth=bool(args.device_auth),
            skip_exchange=bool(args.skip_exchange),
            concurrency=max(1, int(args.concurrency)),
            account_retries=account_retries,
            account_delay=account_delay,
            skip_existing=bool(args.skip_existing),
            skip_emails_file=skip_emails_path,
        )
    )

    ok = [r for r in results if r.get("ok") and r.get("personalToken")]
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
