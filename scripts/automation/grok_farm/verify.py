from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any

from .config import Config

CHAT_MARKER = "ACTIVE"


def chat_response_is_active(payload: dict[str, Any]) -> bool:
    choices = payload.get("choices") or []
    if not choices:
        return False
    message = choices[0].get("message") or {}
    return str(message.get("content") or "").strip() == CHAT_MARKER


def verify_chat(access_token: str, cfg: Config | None = None) -> bool:
    """
    Live chat probe against cli-chat-proxy.grok.com.
    Must return content exactly ACTIVE before treating relogin as success.
    """
    chat_url = (cfg.chat_url if cfg else None) or (
        "https://cli-chat-proxy.grok.com/v1/chat/completions"
    )
    version = (cfg.chat_client_version if cfg else None) or "0.2.114"
    identifier = (cfg.chat_client_identifier if cfg else None) or "grok-shell"
    user_agent = (cfg.chat_user_agent if cfg else None) or "grok-shell/0.2.114 (linux; x86_64)"
    payload = json.dumps(
        {
            "model": "grok-4.5",
            "messages": [{"role": "user", "content": f"Reply with exactly {CHAT_MARKER}"}],
            "max_tokens": 16,
            "stream": False,
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        chat_url,
        data=payload,
        headers={
            "Authorization": f"Bearer {access_token}",
            "Content-Type": "application/json",
            "User-Agent": user_agent,
            "x-xai-token-auth": "xai-grok-cli",
            "x-grok-client-identifier": identifier,
            "x-grok-client-version": version,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=90) as response:
            raw = response.read().decode("utf-8")
            status = response.status
            result = json.loads(raw)
    except urllib.error.HTTPError as exc:
        body = ""
        try:
            body = exc.read().decode("utf-8", errors="replace")[:200]
        except Exception:
            pass
        raise RuntimeError(f"verify_chat HTTP {exc.code}: {body}") from exc
    except Exception as exc:
        raise RuntimeError(f"verify_chat failed: {exc}") from exc

    if status != 200:
        raise RuntimeError(f"verify_chat status {status}")
    if not chat_response_is_active(result):
        content = ""
        try:
            content = str((result.get("choices") or [{}])[0].get("message", {}).get("content") or "")
        except Exception:
            pass
        raise RuntimeError(f"verify_chat content not ACTIVE: {content[:80]!r}")
    return True
