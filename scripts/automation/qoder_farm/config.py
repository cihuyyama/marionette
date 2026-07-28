from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

# Package lives at scripts/automation/qoder_farm/ (flat package root)
_ROOT = Path(__file__).resolve().parent


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


def _env_float_list(key: str, default: str) -> list[float]:
    raw = _env(key, default)
    out: list[float] = []
    for part in raw.split(","):
        part = part.strip()
        if not part:
            continue
        try:
            out.append(float(part))
        except ValueError:
            continue
    return out or [2.0, 5.0, 10.0, 20.0, 30.0]


@dataclass(frozen=True)
class Config:
    root: Path
    headless: bool
    browser_os: str
    login_timeout: int
    proxy_url: str
    proxy_file: str
    proxy_shuffle: bool
    humanize: bool
    inject_settle_secs: int
    dudul_inject: bool
    dudul_url: str
    dudul_access_key: str
    inject_max_attempts: int
    inject_backoff: list[float]
    inject_total_budget_s: int
    output: Path
    screenshot_dir: Path
    ui: str
    debug: bool
    json_progress: bool

    # Qoder endpoints (from etteeum)
    sign_in_url: str = "https://qoder.com/users/sign-in"
    integrations_url: str = "https://qoder.com/account/integrations"
    device_auth_base: str = "https://qoder.com/device/selectAccounts"
    client_id: str = "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb"
    openapi_base: str = "https://openapi.qoder.sh"
    pat_exchange_url: str = "https://openapi.qoder.sh/api/v1/jobToken/exchange"
    quota_url: str = "https://openapi.qoder.sh/api/v2/quota/usage"
    plan_url: str = "https://openapi.qoder.sh/api/v2/user/plan"


def load_config() -> Config:
    _load_dotenv()
    out = Path(_env("QODER_OUTPUT", str(_ROOT / "results" / "qoder-accounts.json")))
    if not out.is_absolute():
        out = _ROOT / out
    shots = Path(_env("QODER_SCREENSHOT_DIR", str(_ROOT / "screenshots")))
    if not shots.is_absolute():
        shots = _ROOT / shots
    return Config(
        root=_ROOT,
        headless=_env_bool("QODER_HEADLESS", False),
        browser_os=_env("QODER_BROWSER_OS", "windows") or "windows",
        login_timeout=_env_int("QODER_LOGIN_TIMEOUT", 120),
        proxy_url=_env("QODER_PROXY_URL"),
        proxy_file=_env("QODER_PROXY_FILE"),
        proxy_shuffle=_env_bool("QODER_PROXY_SHUFFLE", True),
        humanize=_env_bool("QODER_HUMANIZE", True),
        inject_settle_secs=_env_int("QODER_INJECT_SETTLE_SECS", 5),
        dudul_inject=_env_bool("QODER_DUDUL_INJECT", True),
        dudul_url=_env("QODER_DUDUL_URL", "https://dudul.dev/inject")
        or "https://dudul.dev/inject",
        dudul_access_key=_env("QODER_DUDUL_ACCESS_KEY"),
        inject_max_attempts=_env_int("QODER_INJECT_MAX_ATTEMPTS", 8),
        inject_backoff=_env_float_list("QODER_INJECT_BACKOFF", "2,2,2,2,2,2,2"),
        inject_total_budget_s=_env_int("QODER_INJECT_TOTAL_BUDGET_S", 300),
        output=out,
        screenshot_dir=shots,
        ui=(_env("QODER_UI", "log") or "log").lower(),
        debug=_env_bool("QODER_DEBUG", False),
        json_progress=_env_bool("QODER_JSON_PROGRESS", False),
    )
