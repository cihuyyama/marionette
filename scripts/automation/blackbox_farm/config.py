"""Configuration for the Blackbox signup + API-key farm.

Browser flow ported from refs/novabox (MIT); temp-mail backend is OUR
self-hosted cloudflare_temp_email worker (see mail.py), never catchmail.io.
"""
from __future__ import annotations

import os
import random
from dataclasses import dataclass, field
from pathlib import Path

# Package lives at scripts/automation/blackbox_farm/
_ROOT = Path(__file__).resolve().parent

DEFAULT_BLACKBOX_URL = "https://app.blackbox.ai"


def _load_dotenv() -> None:
    try:
        from dotenv import load_dotenv

        load_dotenv(_ROOT / ".env", override=False)
    except ImportError:
        env_path = _ROOT / ".env"
        if not env_path.is_file():
            return
        for line in env_path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, _, v = line.partition("=")
            os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


def _env(key: str, default: str = "") -> str:
    return (os.environ.get(key) or default).strip()


def _env_bool(key: str, default: bool = False) -> bool:
    raw = _env(key, "true" if default else "false").lower()
    return raw in ("1", "true", "yes", "on")


def _env_int(key: str, default: int) -> int:
    try:
        return int(_env(key, str(default)) or default)
    except ValueError:
        return default


def generate_company_name() -> str:
    """Random, realistic-sounding company name for the key-name field.

    Ported 1:1 from refs/novabox/config.py (verified against the live
    app.blackbox.ai key modal).
    """
    list_a = [
        "Blue", "Red", "Green", "Black", "White", "Clear", "Bright", "Swift",
        "Fast", "True", "First", "Next", "Open", "Free", "Smart", "Ever",
        "Drop", "Mail", "Coin", "Snow", "Door", "Air", "Snap", "Bit", "Fire",
        "Ice", "Sky", "Sea", "Moon", "Star", "Sun", "Cloud", "Code", "Data",
        "App", "Web", "Net", "Tech", "Byte", "Crowd", "Base", "Peak", "Blue",
    ]
    list_b = [
        "box", "base", "flare", "chat", "note", "dash", "bnb", "fly", "bird",
        "tree", "wood", "stone", "river", "field", "view", "point", "line",
        "mark", "way", "path", "cast", "flow", "sync", "link", "hub", "deck",
        "space", "time", "wave", "force", "light", "beam", "flare", "wire",
    ]
    last_names = [
        "Smith", "Johnson", "Williams", "Brown", "Jones", "Miller", "Davis",
        "Wilson", "Anderson", "Thomas", "Taylor", "Moore", "Jackson",
        "Martin", "Lee", "Thompson", "White", "Harris", "Clark", "Lewis",
    ]
    traditional_suffixes = [
        "Group", "Partners", "Holdings", "Capital", "Ventures",
        "Consulting", "Associates", "Enterprises", "Logistics", "Management",
    ]
    single_nouns = [
        "Lattice", "Drift", "Loom", "Tide", "Plaid", "Gusto", "Notion",
        "Oyster", "Forge", "Canvas", "Beacon", "Anchor", "Zenith",
        "Compass", "Horizon", "Pinnacle", "Summit", "Vertex", "Apex",
        "Spoke", "Stride", "Plum", "Acorn", "Flock", "Glint", "Tally",
    ]

    pattern = random.choices([1, 2, 3], weights=[5, 3, 2], k=1)[0]

    if pattern == 1:
        # Tech/Startup compound name (Dropbox, Snowflake, Coinbase style)
        return f"{random.choice(list_a)}{random.choice(list_b)}"
    if pattern == 2:
        # Traditional corporate name (Miller Group, Davis Ventures)
        return f"{random.choice(last_names)} {random.choice(traditional_suffixes)}"
    # Single modern noun (Drift, Notion, Plaid)
    return random.choice(single_nouns)


@dataclass(frozen=True)
class Config:
    root: Path
    blackbox_url: str
    headless: bool
    # Per-step browser timeouts (seconds) — novabox request_timeout.
    request_timeout: int
    # How long to poll the temp-mail for the 6-digit OTP.
    otp_timeout: int
    # Poll cadence for the OTP mailbox (novabox verify_poll_interval).
    otp_poll_interval: float
    # Wall-clock budget for one account's whole register pipeline; on expiry
    # the worker marks the account failed and moves on instead of hanging.
    account_timeout: int
    output: Path
    screenshot_dir: Path
    debug: bool
    json_progress: bool
    # Random company-style key name per account (novabox pattern).
    key_name: str = field(default_factory=generate_company_name)
    # Self-hosted cloudflare_temp_email worker (see mail.py). Required for
    # register mode — the runner also injects these from DB mail settings.
    cf_mail_base_url: str = ""
    cf_mail_admin_password: str = ""
    cf_mail_domain: str = ""
    cf_mail_site_password: str = ""


def load_config() -> Config:
    _load_dotenv()
    out = Path(_env("BLACKBOX_OUTPUT", str(_ROOT / "results" / "blackbox-accounts.json")))
    if not out.is_absolute():
        out = _ROOT / out
    shots = Path(_env("BLACKBOX_SCREENSHOT_DIR", str(_ROOT / "screenshots")))
    if not shots.is_absolute():
        shots = _ROOT / shots
    return Config(
        root=_ROOT,
        blackbox_url=_env("BLACKBOX_URL", DEFAULT_BLACKBOX_URL) or DEFAULT_BLACKBOX_URL,
        headless=_env_bool("BLACKBOX_HEADLESS", True),
        request_timeout=_env_int("BLACKBOX_TIMEOUT", 30),
        otp_timeout=_env_int("BLACKBOX_OTP_TIMEOUT", 120),
        otp_poll_interval=3.0,
        account_timeout=_env_int("BLACKBOX_ACCOUNT_TIMEOUT", 600),
        output=out,
        screenshot_dir=shots,
        debug=_env_bool("BLACKBOX_DEBUG", False),
        json_progress=_env_bool("BLACKBOX_JSON_PROGRESS", False),
        cf_mail_base_url=_env("BLACKBOX_CF_MAIL_BASE_URL"),
        cf_mail_admin_password=_env("BLACKBOX_CF_MAIL_ADMIN_PASSWORD"),
        cf_mail_domain=_env("BLACKBOX_CF_MAIL_DOMAIN"),
        cf_mail_site_password=_env("BLACKBOX_CF_MAIL_SITE_PASSWORD"),
    )
