"""Blackbox.ai browser flow driven by plain Playwright Chromium.

Ported 1:1 from refs/novabox/providers/blackbox.py (MIT, verified selectors
against live app.blackbox.ai). One chromium.launch per account; cookies
persist in a single context from signup through key creation. The signup form
is a Next.js server action (multipart POST /signup) that requires a real
browser — httpx cannot reproduce it.

No anti-detect / fingerprint trickery: novabox runs plain headless chromium.
"""
from __future__ import annotations

import asyncio
import re
from typing import Any, Awaitable, Callable

from playwright.async_api import (
    Browser,
    BrowserContext,
    Page,
    TimeoutError as PlaywrightTimeoutError,
    async_playwright,
)

from .config import Config


class BlackboxError(Exception):
    """Raised when a browser step in the Blackbox flow fails (step-prefixed)."""


class BlackboxClient:
    """Owns one Playwright browser/context/page for the whole account flow."""

    def __init__(self, cfg: Config) -> None:
        self._cfg = cfg
        self._playwright = None
        self._browser: Browser | None = None
        self._context: BrowserContext | None = None
        self._page: Page | None = None
        self._key_created: asyncio.Event = asyncio.Event()
        self._api_key: str = ""
        self._email: str = ""

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        try:
            self._playwright = await async_playwright().start()
            self._browser = await self._playwright.chromium.launch(
                headless=self._cfg.headless,
            )
            self._context = await self._browser.new_context()
            self._page = await self._context.new_page()
        except Exception as exc:
            raise BlackboxError(f"launch: chromium launch failed: {exc}") from exc

        # Block images, fonts, media to save RAM and speed up.
        await self._page.route(
            "**/*",
            lambda route: (
                route.abort()
                if route.request.resource_type in ("image", "media", "font")
                else route.continue_()
            ),
        )

    async def stop(self) -> None:
        try:
            if self._browser is not None:
                await self._browser.close()
        except Exception:
            pass
        finally:
            self._browser = None
            self._context = None
            self._page = None
            if self._playwright is not None:
                try:
                    await self._playwright.stop()
                except Exception:
                    pass
                self._playwright = None

    @property
    def page(self) -> Page:
        if self._page is None:
            raise BlackboxError("launch: client not started")
        return self._page

    # ------------------------------------------------------------------
    # Full flow
    # ------------------------------------------------------------------

    async def register_and_create_key(
        self,
        email: str,
        password: str,
        wait_otp: Callable[[str], Awaitable[str]],
        on_step: Callable[[str], None] | None = None,
    ) -> str:
        """Run the entire verified flow and return the sk-... API key.

        Steps:
          1. Open /signup, fill email+password, submit (server action POST /signup)
          2. Wait for the OTP mail (wait_otp polls our CF temp-mail worker)
          3. Enter the code, click Verify, land on /activity
          4. Navigate to /keys, CREATE KEY, name it, confirm, capture key
          5. Click DONE to close the modal
        """
        self._email = email
        page = self.page
        page.set_default_timeout(self._cfg.request_timeout * 1000)

        if on_step:
            on_step("signing up...")
        await self.signup(email, password)

        if on_step:
            on_step("waiting for otp...")
        code = await wait_otp(email)
        if not code:
            await self._shot(email, "fail_wait_otp")
            raise BlackboxError(
                f"wait_otp: no 6-digit OTP for {email} within {self._cfg.otp_timeout}s"
            )

        if on_step:
            on_step("verifying otp...")
        await self.verify_otp(code)

        if on_step:
            on_step("creating api key...")
        api_key = await self.create_api_key()

        if on_step:
            on_step("done")
        return api_key

    # ------------------------------------------------------------------
    # Step 1 — signup
    # ------------------------------------------------------------------

    async def signup(self, email: str, password: str) -> None:
        page = self.page
        try:
            await page.goto(
                f"{self._cfg.blackbox_url}/signup", wait_until="domcontentloaded"
            )
        except Exception as exc:
            await self._shot(email, "fail_signup_goto")
            raise BlackboxError(f"signup: goto /signup failed: {exc}") from exc

        try:
            email_input = page.locator('input[type="email"], input[name="email"]').first
            await email_input.wait_for(state="visible", timeout=30_000)
            await email_input.fill(email)

            pass_input = page.locator('input[type="password"], input[name="password"]').first
            await pass_input.wait_for(state="visible", timeout=10_000)
            await pass_input.fill(password)

            # The form is a Next.js server action — clicking the submit button
            # fires the multipart POST /signup captured in the network log.
            submit = page.locator('button[type="submit"]').first
            await submit.click()
        except Exception as exc:
            await self._shot(email, "fail_signup_fill")
            raise BlackboxError(f"signup: fill/submit failed: {exc}") from exc

        try:
            # Give the server action a moment to round-trip before the OTP screen.
            await _wait_any(
                page,
                ["text=Verify", "input[maxlength='6']", "text=verification", "text=code"],
                timeout=15,
                hint="OTP screen after signup",
            )
        except BlackboxError:
            await self._shot(email, "fail_signup_otp_screen")
            raise

    # ------------------------------------------------------------------
    # Step 2 — OTP verification
    # ------------------------------------------------------------------

    async def verify_otp(self, code: str) -> None:
        page = self.page
        try:
            otp_input = page.locator(
                'input[maxlength="6"], input[placeholder*="code" i], '
                'input[name="code"], input[inputmode="numeric"]'
            ).first
            await otp_input.wait_for(state="visible", timeout=15_000)
            await otp_input.fill(code)

            verify_btn = page.locator('button:has-text("Verify")').first
            await verify_btn.click()
        except Exception as exc:
            await self._shot(self._email, "fail_verify_otp")
            raise BlackboxError(f"verify_otp: fill/click failed: {exc}") from exc

        # After verification the app auto-logs-in and lands on /activity.
        # wait_for_url's default 'load' event can stall on the SPA, so poll.
        deadline = asyncio.get_event_loop().time() + 45
        while asyncio.get_event_loop().time() < deadline:
            if re.search(r"/(activity|dashboard)", page.url):
                return
            await asyncio.sleep(0.5)
        await self._shot(self._email, "fail_verify_land")
        raise BlackboxError(
            f"verify_otp: did not reach /activity after verify (still at {page.url})"
        )

    # ------------------------------------------------------------------
    # Step 3 — API key creation
    # ------------------------------------------------------------------

    async def create_api_key(self, name: str | None = None) -> str:
        key_name = name or self._cfg.key_name
        page = self.page

        # Listen for the key POST response in case the modal read-back fails.
        self._key_created = asyncio.Event()
        self._api_key = ""
        page.on(
            "response",
            lambda r: asyncio.create_task(self._capture_key_response(r)),
        )

        try:
            await page.goto(f"{self._cfg.blackbox_url}/keys", wait_until="domcontentloaded")
            create_btn = page.locator('button:has-text("CREATE KEY")').first
            await create_btn.wait_for(state="visible", timeout=30_000)
            await create_btn.click()

            # Modal appears with a key-name input (placeholder "e.g. Production")
            # and a disabled "Create API Key" button until a name is entered.
            name_locator = page.locator(
                'input[placeholder*="Production"], input[placeholder*="Key name"], '
                'input[placeholder*="e.g."]'
            ).first
            await name_locator.wait_for(state="visible", timeout=15_000)
            await name_locator.fill(key_name)

            confirm_btn = page.locator(
                'button:has-text("CREATE API KEY"), button:has-text("Create API Key")'
            ).first
            await confirm_btn.wait_for(state="visible", timeout=15_000)
            # The button starts disabled and enables once the name is non-empty;
            # wait for it to become enabled before clicking.
            await page.wait_for_function(
                """() => {
                    const btns = [...document.querySelectorAll('button')];
                    return btns.some(b => /create api key/i.test(b.textContent || '') && !b.disabled);
                }""",
                timeout=15_000,
            )
            await confirm_btn.click()
        except Exception as exc:
            await self._shot(self._email, "fail_create_key")
            raise BlackboxError(f"create_key: modal flow failed: {exc}") from exc

        # The key appears in a modal. Prefer reading it from the network
        # response, then fall back to scanning the page text.
        api_key = ""
        try:
            await asyncio.wait_for(self._key_created.wait(), timeout=15)
            api_key = self._api_key
        except asyncio.TimeoutError:
            pass

        if not api_key:
            api_key = await self._read_key_from_page()

        if not api_key:
            await self._shot(self._email, "fail_key_not_found")
            raise BlackboxError("create_key: API key not found after creation")

        await self._close_key_modal()
        return api_key

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    async def _capture_key_response(self, response: Any) -> None:
        try:
            url = response.url
            if url.endswith("/api/v0/keys") or "/api/v0/keys?" in url:
                if response.request.method == "POST":
                    body = await response.text()
                    match = re.search(r'"(?:api_key|key|token)"\s*:\s*"([^"]+)"', body)
                    if match:
                        self._api_key = match.group(1)
                        self._key_created.set()
        except Exception:
            pass

    async def _read_key_from_page(self) -> str:
        page = self.page
        for _ in range(5):
            try:
                text = await page.locator("body").inner_text()
            except Exception:
                text = ""
            for pattern in (r"sk-[A-Za-z0-9_-]{12,}", r"\b(?:bb_|sk_)[A-Za-z0-9_-]{16,}\b"):
                match = re.search(pattern, text)
                if match:
                    return match.group(0)
            await asyncio.sleep(1)
        return ""

    async def _close_key_modal(self) -> None:
        page = self.page
        done = page.locator(
            'button:has-text("DONE"), button:has-text("Done"), button:has-text("Close")'
        ).first
        try:
            await done.click(timeout=5_000)
        except PlaywrightTimeoutError:
            # Modal already closed or no close button — nothing to do.
            pass
        except Exception:
            pass

    async def _shot(self, email: str, tag: str) -> None:
        """Best-effort failure screenshot (grok_farm _shot pattern)."""
        try:
            page = self._page
            if page is None:
                return
            self._cfg.screenshot_dir.mkdir(parents=True, exist_ok=True)
            safe = (email or "unknown").replace("@", "_at_").replace(".", "_")
            path = self._cfg.screenshot_dir / f"{safe}_{tag}.png"
            await page.screenshot(path=str(path), full_page=True)
        except Exception:
            pass


async def _wait_any(
    page: Page,
    selectors: list[str],
    *,
    timeout: float,
    hint: str,
) -> None:
    """Wait until any of the selectors matches, or raise BlackboxError."""
    deadline = asyncio.get_event_loop().time() + timeout
    while asyncio.get_event_loop().time() < deadline:
        for sel in selectors:
            locator = page.locator(sel)
            try:
                if await locator.count() > 0 and await locator.first.is_visible():
                    return
            except Exception:
                continue
        await asyncio.sleep(0.5)
    raise BlackboxError(f"signup: timed out waiting for {hint}")
