"""Post-harvest key validation against the real Blackbox API.

Blocking urllib (no new deps) — run inside an executor from async code.
A fresh free-tier key must answer a trivial chat completion; anything else
means the key is dead-on-arrival and the account should be marked failed.
"""
from __future__ import annotations

import json
import urllib.error
import urllib.request

VALIDATE_URL = "https://api.blackbox.ai/v1/chat/completions"
VALIDATE_MODEL = "blackboxai/mistral/ministral-3b"
VALIDATE_TIMEOUT = 40  # seconds


class ValidationError(Exception):
    """Raised when the harvested key fails its first live chat probe."""


def validate_key(api_key: str) -> bool:
    """POST a minimal chat completion; True on 200 with non-empty choices.

    Raises ValidationError with a status/body snippet otherwise.
    """
    payload = {
        "model": VALIDATE_MODEL,
        "messages": [{"role": "user", "content": "Say OK"}],
        "max_tokens": 8,
    }
    req = urllib.request.Request(VALIDATE_URL, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("Authorization", f"Bearer {api_key}")
    data = json.dumps(payload).encode("utf-8")
    try:
        with urllib.request.urlopen(req, data=data, timeout=VALIDATE_TIMEOUT) as resp:
            body = resp.read().decode("utf-8", errors="replace")
            status = resp.status
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")[:200]
        raise ValidationError(f"validate: HTTP {exc.code}: {detail}") from exc
    except Exception as exc:
        raise ValidationError(f"validate: request failed: {exc}") from exc

    if status != 200:
        raise ValidationError(f"validate: HTTP {status}: {body[:200]}")
    try:
        parsed = json.loads(body)
    except json.JSONDecodeError as exc:
        raise ValidationError(f"validate: bad JSON: {body[:200]}") from exc
    choices = parsed.get("choices")
    if not isinstance(choices, list) or not choices:
        raise ValidationError(f"validate: empty choices: {body[:200]}")
    return True
