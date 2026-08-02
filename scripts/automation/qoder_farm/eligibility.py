from __future__ import annotations

import math
from typing import Any


_FREE_PLANS = frozenset({"", "community", "free", "free plan", "personal_standard", "standard"})


def inject_eligibility(quota: dict[str, Any] | None) -> tuple[bool, str]:
    if (
        not isinstance(quota, dict)
        or "quotaLimit" not in quota
        or "quotaRemaining" not in quota
        or "plan" not in quota
    ):
        return False, "quota unverifiable"

    try:
        limit = float(quota["quotaLimit"])
    except (TypeError, ValueError):
        return False, "quota limit unverifiable"

    if not math.isfinite(limit) or limit < 0:
        return False, "quota limit unverifiable"

    if limit > 0:
        return (
            False,
            "prior credit bucket "
            f"(limit={quota.get('quotaLimit')} remaining={quota.get('quotaRemaining')} plan={quota.get('plan')})",
        )

    plan = str(quota.get("plan") or "").strip().lower()
    if plan not in _FREE_PLANS:
        return False, f"prior trial or paid plan ({quota.get('plan')})"

    return True, "fresh free account"
