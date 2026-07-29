from __future__ import annotations

import asyncio
import os
import random
import re
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from .config import Config
from .progress import Progress

_proxy_idx = 0
_proxy_lock = asyncio.Lock()


def _normalize_proxy_url(raw: str) -> str | None:
    s = (raw or "").strip()
    if not s:
        return None
    if (s.startswith('"') and s.endswith('"')) or (s.startswith("'") and s.endswith("'")):
        s = s[1:-1].strip()
    if not s:
        return None
    if " #" in s:
        s = s.split(" #", 1)[0].strip()
    if "://" in s:
        return s
    parts = s.split(":")
    if len(parts) >= 4 and parts[1].isdigit() and "@" not in parts[0]:
        host, port, user = parts[0], parts[1], parts[2]
        password = ":".join(parts[3:])
        if host and user:
            return f"http://{user}:{password}@{host}:{port}"
    if "@" in s:
        return f"http://{s}"
    if len(parts) == 2 and parts[1].isdigit():
        return f"http://{parts[0]}:{parts[1]}"
    return None


def load_proxy_pool(cfg: Config) -> list[str]:
    pool: list[str] = []
    file_env = (cfg.proxy_file or os.getenv("GROK_PROXY_FILE") or "").strip()
    paths: list[Path] = []
    if file_env:
        p = Path(file_env).expanduser()
        if not p.is_absolute():
            p = (cfg.root / p).resolve()
        paths.append(p)
    default = (cfg.root / "proxies.txt").resolve()
    if default not in paths:
        paths.append(default)
    for path in paths:
        if not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for line in text.splitlines():
            raw = line.strip()
            if not raw or raw.startswith("#"):
                continue
            url = _normalize_proxy_url(raw)
            if url:
                pool.append(url)
        if pool:
            break

    single = (cfg.proxy_url or os.getenv("BATCHER_PROXY_URL") or "").strip()
    if single:
        url = _normalize_proxy_url(single)
        if url and url not in pool:
            pool.insert(0, url)

    inline = (os.getenv("GROK_PROXY_POOL") or "").strip()
    if inline:
        for item in inline.split(","):
            url = _normalize_proxy_url(item.strip())
            if url and url not in pool:
                pool.append(url)

    if cfg.proxy_shuffle and len(pool) > 1:
        random.shuffle(pool)
    return pool


async def next_proxy_url(cfg: Config) -> str | None:
    global _proxy_idx
    pool = load_proxy_pool(cfg)
    if not pool:
        return None
    async with _proxy_lock:
        url = pool[_proxy_idx % len(pool)]
        _proxy_idx += 1
        return url


def _proxy_dict(proxy_url: str) -> dict[str, Any]:
    if "://" not in proxy_url:
        proxy_url = f"http://{proxy_url}"
    parsed = urlparse(proxy_url)
    scheme = (parsed.scheme or "http").lower()
    server = f"{scheme}://{parsed.hostname}"
    if parsed.port:
        server += f":{parsed.port}"
    out: dict[str, Any] = {"server": server}
    if parsed.username:
        out["username"] = parsed.username
    if parsed.password:
        out["password"] = parsed.password
    return out


async def launch_camoufox(cfg: Config, prog: Progress) -> dict[str, Any]:
    try:
        from browserforge.fingerprints import Screen
        from camoufox.async_api import AsyncCamoufox
    except Exception as exc:
        raise RuntimeError(
            f"camoufox import failed: {exc}. "
            "pip install -r requirements.txt && python -m camoufox fetch"
        ) from exc

    humanize: Any = False
    if cfg.humanize:
        humanize = cfg.humanize_headless if cfg.headless else cfg.humanize_headed

    kwargs: dict[str, Any] = {
        "headless": cfg.headless,
        "os": cfg.browser_os,
        "block_webrtc": True,
        "humanize": humanize,
        "locale": "en-US",
        "screen": Screen(max_width=1920, max_height=1080),
        "geoip": True,
        "disable_coop": True,
        "i_know_what_im_doing": True,
    }

    proxy_url = await next_proxy_url(cfg)
    if proxy_url:
        kwargs["proxy"] = _proxy_dict(proxy_url)
        kwargs["geoip"] = True
        parsed = urlparse(proxy_url if "://" in proxy_url else f"http://{proxy_url}")
        prog.log(f"proxy {parsed.hostname}:{parsed.port}", "INFO")

    manager = AsyncCamoufox(**kwargs)
    browser = await manager.__aenter__()
    page = await browser.new_page()
    page.set_default_timeout(max(60_000, cfg.login_timeout * 1000))
    return {
        "manager": manager,
        "browser": browser,
        "page": page,
        "proxy_url": proxy_url or None,
    }


async def close_session(session: dict[str, Any] | None) -> None:
    if not session:
        return
    manager = session.get("manager")
    if manager:
        try:
            await manager.__aexit__(None, None, None)
        except Exception:
            pass


async def recover_page_load_error(page: Any) -> bool:
    """Handle Firefox 'This page couldn't load' blips."""
    try:
        body = (await page.inner_text("body"))[:500].lower()
    except Exception:
        body = ""
    markers = (
        "couldn't load",
        "could not load",
        "page isn’t available",
        "page isn't available",
        "can't be reached",
        "cannot be reached",
        "took too long",
    )
    if not any(m in body for m in markers):
        if "reload" in body and ("try again" in body or "problem" in body):
            pass
        else:
            return False
    try:
        btn = page.get_by_role("button", name=re.compile(r"reload|try again", re.I))
        if await btn.count() > 0:
            await btn.first.click(timeout=3000)
        else:
            await page.reload(wait_until="domcontentloaded", timeout=45_000)
        await asyncio.sleep(1.5)
        return True
    except Exception:
        try:
            await page.reload(wait_until="domcontentloaded", timeout=45_000)
            await asyncio.sleep(1.5)
            return True
        except Exception:
            return False
