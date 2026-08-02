from __future__ import annotations

import asyncio
import re
import secrets
import string
from typing import Any

from .captcha import handle_captcha
from .config import Config
from .imap import read_code
from .progress import Progress


FIRST_NAMES = [
    "Alex", "Jordan", "Taylor", "Morgan", "Casey", "Riley", "Quinn", "Avery",
    "Parker", "Sage", "River", "Skyler", "Dakota", "Reese", "Finley", "Rowan",
    "Charlie", "Emerson", "Hayden", "Jamie", "Blake", "Drew", "Eden", "Kai",
    "Noah", "Liam", "Emma", "Olivia", "Mia", "Lucas", "Mason", "Sophia",
    "Ethan", "Ava", "Leo", "Isla", "Nora", "Ezra", "Milo", "Ruby",
    "Owen", "Iris", "Felix", "Luna", "Hugo", "Nina", "Theo", "Elena",
    "Adrian", "Clara", "Julian", "Maya", "Simon", "Vera", "Oscar", "Cora",
]
LAST_NAMES = [
    "Smith", "Johnson", "Williams", "Brown", "Jones", "Garcia", "Miller",
    "Davis", "Rodriguez", "Martinez", "Anderson", "Taylor", "Thomas", "Moore",
    "Jackson", "Martin", "Lee", "Thompson", "White", "Harris", "Clark", "Lewis",
    "Walker", "Hall", "Allen", "Young", "King", "Wright", "Scott", "Green",
    "Baker", "Adams", "Nelson", "Carter", "Mitchell", "Perez", "Roberts",
    "Turner", "Phillips", "Campbell", "Parker", "Evans", "Edwards", "Collins",
    "Reed", "Cook", "Morgan", "Bell", "Murphy", "Bailey", "Rivera", "Cooper",
]


def random_name() -> tuple[str, str]:
    return secrets.choice(FIRST_NAMES), secrets.choice(LAST_NAMES)


def generate_email(source: str, first: str = "", last: str = "") -> str:
    if first and last:
        seps = [".", "", "_"]
        local = f"{first.lower()}{secrets.choice(seps)}{last.lower()}{secrets.randbelow(1000)}"
    else:
        local = "".join(secrets.choice(string.ascii_lowercase + string.digits) for _ in range(14))
    if "@" in source:
        base, _, domain = source.partition("@")
        return f"{base}+{local}@{domain}"
    return f"{local}@{source}"


async def _fill(page: Any, selectors: str, value: str) -> bool:
    try:
        loc = page.locator(selectors).first
        if await loc.count() > 0:
            await loc.fill(value)
            return True
    except Exception:
        pass
    return False


async def _click_continue(page: Any) -> bool:
    try:
        btn = page.get_by_role("button", name=re.compile(r"continue", re.I))
        if await btn.count() > 0:
            await btn.first.click()
            return True
    except Exception:
        pass
    return False


async def fill_signup_basic(
    page: Any, email: str, prog: Progress, first: str = "", last: str = ""
) -> bool:
    if not first or not last:
        first, last = random_name()
    first_sel = 'input#basic_firstName, input[placeholder*="first name" i]'
    try:
        await page.locator(first_sel).first.wait_for(state="visible", timeout=30_000)
    except Exception:
        prog.log("signup form did not render", "ERR", email=email, step="signup")
        return False
    ok = await _fill(page, first_sel, first)
    if not ok:
        prog.log("first name field not found", "ERR", email=email, step="signup")
        return False
    await _fill(
        page,
        'input#basic_lastName, input[placeholder*="Last Name" i]',
        last,
    )
    if not await _fill(
        page,
        'input#basic_email, input[placeholder*="email" i]',
        email,
    ):
        prog.log("email field not found", "ERR", email=email, step="signup")
        return False
    try:
        cb = page.locator('input.ant-checkbox-input, input[type="checkbox"]').first
        if await cb.count() > 0 and not await cb.is_checked():
            await cb.check()
    except Exception:
        pass
    await asyncio.sleep(0.4)
    await _click_continue(page)
    await asyncio.sleep(1.5)
    return True


async def fill_password_step(page: Any, password: str, prog: Progress, email: str) -> bool:
    for _ in range(12):
        if await _fill(
            page, 'input#basic_password, input[type="password"]', password
        ):
            await asyncio.sleep(0.4)
            await _click_continue(page)
            await asyncio.sleep(1.5)
            return True
        await asyncio.sleep(0.8)
    prog.log("password field not found", "ERR", email=email, step="signup")
    return False


async def _click_verify_gate(page: Any) -> None:
    try:
        gate = page.get_by_text(re.compile(r"click to verify", re.I)).first
        if await gate.count() > 0:
            await gate.click()
            await asyncio.sleep(2.5)
    except Exception:
        pass


async def _otp_boxes(page: Any) -> Any:
    # qoder's verify step renders 6 separate single-char boxes (Ant Design OTP:
    # type=text, no maxlength). Prefer an explicit OTP group, else fall back to a
    # visible row of exactly 6 short text inputs that are NOT the name/email/password
    # fields from earlier steps.
    return await page.evaluate(
        """() => {
            const vis = el => el && el.offsetParent !== null;
            let group = document.querySelector('.ant-otp, [class*="otp" i], [class*="verification" i]');
            let inputs;
            if (group) {
                inputs = [...group.querySelectorAll('input')].filter(vis);
            } else {
                inputs = [...document.querySelectorAll('input')].filter(i =>
                    vis(i) && (i.type === 'text' || i.type === 'tel' || i.type === 'number')
                    && !i.id.includes('firstName') && !i.id.includes('lastName')
                    && !i.id.includes('email') && i.type !== 'password');
            }
            return inputs.length;
        }"""
    )


async def fill_otp(page: Any, code: str, prog: Progress, email: str) -> bool:
    for _ in range(8):
        try:
            single = page.locator(
                'input[autocomplete="one-time-code"], input[placeholder*="code" i], input[name*="code" i]'
            )
            if await single.count() > 0:
                await single.first.fill(code)
                await asyncio.sleep(0.3)
                await _click_continue(page)
                return True
            count = await _otp_boxes(page)
            if count >= 6:
                boxes = page.locator(
                    '.ant-otp input, [class*="otp" i] input, input[type="text"], input[type="tel"]'
                )
                await boxes.first.click()
                await page.keyboard.type(code, delay=80)
                await asyncio.sleep(0.5)
                await _click_continue(page)
                return True
        except Exception:
            pass
        await asyncio.sleep(1.0)
    prog.log("OTP field not found", "ERR", email=email, step="otp")
    return False


async def signup_with_email(
    page: Any,
    email: str,
    password: str,
    cfg: Config,
    prog: Progress,
    first: str = "",
    last: str = "",
) -> bool:
    prog.step(email, "signup", "open sign-up")
    await page.goto(cfg.sign_up_url, wait_until="domcontentloaded", timeout=45_000)
    await asyncio.sleep(1.5)

    if not await fill_signup_basic(page, email, prog, first, last):
        return False
    if not await fill_password_step(page, password, prog, email):
        return False

    prog.step(email, "captcha", "slide puzzle")
    await _click_verify_gate(page)
    if not await handle_captcha(page, cfg, prog, email):
        return False
    await asyncio.sleep(2.0)

    prog.step(email, "otp", "await email code")
    loop = asyncio.get_event_loop()
    code = await loop.run_in_executor(None, lambda: read_code(cfg, email, prog, 180))
    if not code:
        prog.log("no OTP received", "ERR", email=email, step="otp")
        return False
    prog.log(f"OTP {code}", "OK", email=email, step="otp")
    if not await fill_otp(page, code, prog, email):
        return False
    await asyncio.sleep(3.0)
    return True
