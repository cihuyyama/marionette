from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

# Package lives at scripts/automation/grok_farm/
_ROOT = Path(__file__).resolve().parent

# Grok CLI OAuth (matches grok-relogin-kit / Marionette default client)
DEFAULT_CLIENT_ID = "b1a00492-073a-47ea-816f-4c329264a828"
DEFAULT_AUTHORIZE = "https://auth.x.ai/oauth2/authorize"
DEFAULT_TOKEN = "https://auth.x.ai/oauth2/token"
DEFAULT_REDIRECT_URI = "http://127.0.0.1:56121/callback"
DEFAULT_SCOPE = (
    "openid profile email offline_access "
    "grok-cli:access api:access conversations:read conversations:write"
)
DEFAULT_SIGNIN = "https://accounts.x.ai/sign-in"
DEFAULT_CHAT_URL = "https://cli-chat-proxy.grok.com/v1/chat/completions"
DEFAULT_CHAT_VERSION = "0.2.114"
DEFAULT_CLIENT_IDENTIFIER = "grok-shell"
DEFAULT_USER_AGENT = "grok-shell/0.2.114 (linux; x86_64)"


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


def _env_float(key: str, default: float) -> float:
    try:
        return float(_env(key, str(default)) or default)
    except ValueError:
        return default


@dataclass(frozen=True)
class Config:
    root: Path
    headless: bool
    browser_os: str
    login_timeout: int
    oauth_timeout: int
    oauth_retries: int
    proxy_url: str
    proxy_file: str
    proxy_shuffle: bool
    humanize: bool
    humanize_headed: float
    humanize_headless: float
    output: Path
    screenshot_dir: Path
    ui: str
    debug: bool
    json_progress: bool
    skip_verify: bool
    # OAuth / endpoints
    client_id: str = DEFAULT_CLIENT_ID
    authorize_url: str = DEFAULT_AUTHORIZE
    token_url: str = DEFAULT_TOKEN
    redirect_uri: str = DEFAULT_REDIRECT_URI
    scope: str = DEFAULT_SCOPE
    signin_url: str = DEFAULT_SIGNIN
    chat_url: str = DEFAULT_CHAT_URL
    chat_client_version: str = DEFAULT_CHAT_VERSION
    chat_client_identifier: str = DEFAULT_CLIENT_IDENTIFIER
    chat_user_agent: str = DEFAULT_USER_AGENT
    # Optional IMAP when xAI shows OTP (not required for pure password accounts)
    imap_host: str = ""
    imap_port: int = 993
    imap_user: str = ""
    imap_pass: str = ""

    @property
    def imap_configured(self) -> bool:
        return bool(self.imap_host and self.imap_user and self.imap_pass)


def load_config() -> Config:
    _load_dotenv()
    out = Path(_env("GROK_OUTPUT", str(_ROOT / "results" / "grok-accounts.json")))
    if not out.is_absolute():
        out = _ROOT / out
    shots = Path(_env("GROK_SCREENSHOT_DIR", str(_ROOT / "screenshots")))
    if not shots.is_absolute():
        shots = _ROOT / shots
    return Config(
        root=_ROOT,
        headless=_env_bool("GROK_HEADLESS", False),
        browser_os=_env("GROK_BROWSER_OS", "windows") or "windows",
        login_timeout=_env_int("GROK_LOGIN_TIMEOUT", 120),
        oauth_timeout=_env_int("GROK_OAUTH_TIMEOUT", 120),
        oauth_retries=_env_int("GROK_OAUTH_RETRIES", 2),
        proxy_url=_env("GROK_PROXY_URL") or _env("BATCHER_PROXY_URL"),
        proxy_file=_env("GROK_PROXY_FILE"),
        proxy_shuffle=_env_bool("GROK_PROXY_SHUFFLE", True),
        humanize=_env_bool("GROK_HUMANIZE", True),
        humanize_headed=_env_float("GROK_HUMANIZE_HEADED", 1),
        humanize_headless=_env_float("GROK_HUMANIZE_HEADLESS", 1),
        output=out,
        screenshot_dir=shots,
        ui=(_env("GROK_UI", "log") or "log").lower(),
        debug=_env_bool("GROK_DEBUG", False),
        json_progress=_env_bool("GROK_JSON_PROGRESS", False),
        skip_verify=_env_bool("GROK_SKIP_VERIFY", False),
        client_id=_env("GROK_CLIENT_ID", DEFAULT_CLIENT_ID) or DEFAULT_CLIENT_ID,
        authorize_url=_env("GROK_AUTHORIZE_URL", DEFAULT_AUTHORIZE) or DEFAULT_AUTHORIZE,
        token_url=_env("GROK_TOKEN_URL", DEFAULT_TOKEN) or DEFAULT_TOKEN,
        redirect_uri=_env("GROK_REDIRECT_URI", DEFAULT_REDIRECT_URI) or DEFAULT_REDIRECT_URI,
        scope=_env("GROK_SCOPE", DEFAULT_SCOPE) or DEFAULT_SCOPE,
        signin_url=_env("GROK_SIGNIN_URL", DEFAULT_SIGNIN) or DEFAULT_SIGNIN,
        chat_url=_env("GROK_CHAT_URL", DEFAULT_CHAT_URL) or DEFAULT_CHAT_URL,
        chat_client_version=_env("GROK_CHAT_CLIENT_VERSION", DEFAULT_CHAT_VERSION)
        or DEFAULT_CHAT_VERSION,
        chat_client_identifier=_env("GROK_CHAT_CLIENT_IDENTIFIER", DEFAULT_CLIENT_IDENTIFIER)
        or DEFAULT_CLIENT_IDENTIFIER,
        chat_user_agent=_env("GROK_CHAT_USER_AGENT", DEFAULT_USER_AGENT) or DEFAULT_USER_AGENT,
        imap_host=_env("GROK_IMAP_HOST", "imap.gmail.com"),
        imap_port=_env_int("GROK_IMAP_PORT", 993),
        imap_user=_env("GROK_IMAP_USER"),
        # Gmail App Passwords display as "abcd efgh ijkl mnop" but IMAP LOGIN rejects spaces.
        imap_pass=_env("GROK_IMAP_PASS").replace(" ", ""),
    )
