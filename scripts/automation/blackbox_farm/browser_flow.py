"""Blackbox.ai browser flow driven by Camoufox (anti-detect Firefox).

Flow ported from refs/novabox/providers/blackbox.py (MIT, verified selectors
against live app.blackbox.ai), then hardened from live runs:

1. Engine = Camoufox with humanize (plain Chromium got OTP delivery refused).
2. OTP: the first verification email often never arrives on its own — the
   "Resend" link on the OTP screen triggers actual delivery. Poll short
   windows, click resend, repeat until the total OTP budget runs out.
3. Key creation intermittently returns nothing; retry the modal flow several
   times (manual testing confirms it takes a few tries).
4. Console + app.blackbox.ai HTTP traffic is captured and surfaced through
   the injected log function so failures are diagnosable from the job log.

The signup form is a Next.js server action (multipart POST /signup) that
requires a real browser — httpx cannot reproduce it.
"""
from __future__ import annotations

import asyncio
import re
from typing import Any, Awaitable, Callable
from urllib.parse import urlparse

from playwright.async_api import (
    Browser,
    BrowserContext,
    Page,
    TimeoutError as PlaywrightTimeoutError,
)

from .config import Config


class BlackboxError(Exception):
    """Raised when a browser step in the Blackbox flow fails (step-prefixed)."""


class _ProgressLogAdapter:
    """Minimal prog-shaped logger for the google_sso driver.

    google_sso.drive_google_auth expects an object with
    .log(msg, level, email=..., step=...) — this adapter forwards to the
    client's injected log_fn and swallows the extra kwargs.
    """

    def __init__(self, log_fn: Callable[[str, str], None] | None) -> None:
        self._log_fn = log_fn

    def log(self, msg: str, level: str = "INFO", **_: Any) -> None:
        if self._log_fn is not None:
            try:
                self._log_fn(msg, level)
            except Exception:
                pass


_OTP_POLL_WINDOW = 30.0
_FIRST_DELIVERY_WINDOW = 45.0
_AFTER_RESEND_WINDOW = 60.0
_500_RETRY_WAIT = 25.0


class BlackboxClient:
    """Owns one Camoufox browser/context/page for the whole account flow."""

    def __init__(self, cfg: Config, log_fn: Callable[[str, str], None] | None = None) -> None:
        self._cfg = cfg
        self._log_fn = log_fn
        self._manager = None
        self._browser: Browser | None = None
        self._context: BrowserContext | None = None
        self._page: Page | None = None
        self._key_created: asyncio.Event = asyncio.Event()
        self._api_key: str = ""
        self._email: str = ""
        self._verification_resp: dict[str, Any] = {}

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        from camoufox.async_api import AsyncCamoufox

        humanize_val = self._cfg.humanize_headed if not self._cfg.headless else self._cfg.humanize_headless
        launch_kwargs: dict[str, Any] = {
            "headless": self._cfg.headless,
            "humanize": humanize_val if self._cfg.humanize else False,
            "os": self._cfg.browser_os,
            "locale": "en-US",
            "disable_coop": True,
            "i_know_what_im_doing": True,
            # The signup page asks "Allow app.blackbox.ai to access your
            # location?" — auto-allow it via prefs so the browser-UI prompt
            # never blocks the flow (Camoufox spoofs the coords anyway).
            "firefox_user_prefs": {
                "geo.prompt.testing.always_allow": True,
                "permissions.default.geo": 1,
            },
        }
        try:
            self._manager = AsyncCamoufox(**launch_kwargs)
            self._browser = await self._manager.__aenter__()
            self._context = await self._browser.new_context()
            self._page = await self._context.new_page()
        except Exception as exc:
            raise BlackboxError(f"launch: camoufox launch failed: {exc}") from exc

        # Block images, fonts, media to save RAM and speed up.
        await self._page.route(
            "**/*",
            lambda route: (
                route.abort()
                if route.request.resource_type in ("image", "media", "font")
                else route.continue_()
            ),
        )
        self._attach_listeners()

    async def stop(self) -> None:
        try:
            if self._manager is not None:
                await self._manager.__aexit__(None, None, None)
        except Exception:
            pass
        finally:
            self._manager = None
            self._browser = None
            self._context = None
            self._page = None

    @property
    def page(self) -> Page:
        if self._page is None:
            raise BlackboxError("launch: client not started")
        return self._page

    # ------------------------------------------------------------------
    # Instrumentation (console + HTTP visibility for the job log)
    # ------------------------------------------------------------------

    def _log(self, msg: str, level: str = "INFO") -> None:
        if self._log_fn is not None:
            try:
                self._log_fn(msg, level)
            except Exception:
                pass

    def _attach_listeners(self) -> None:
        page = self.page
        page.on("console", self._on_console)
        page.on("response", lambda r: asyncio.create_task(self._on_response(r)))
        page.on("requestfailed", self._on_request_failed)

    def _on_console(self, msg: Any) -> None:
        try:
            mtype = msg.type
            if mtype not in ("error", "warning"):
                return
            text = (msg.text or "")[:220]
            if text:
                self._log(f"console[{mtype}] {text}", "WARN")
        except Exception:
            pass

    async def _on_response(self, response: Any) -> None:
        try:
            url = response.url
            if "blackbox.ai" not in url:
                return
            path = urlparse(url).path or "/"
            method = response.request.method
            status = response.status
            level = "DBG" if status < 400 else "WARN"
            self._log(f"http {method} {path} -> {status}", level)
            if "send-verification" in path:
                # The resend loop needs the live status/body to decide waits.
                body = ""
                try:
                    body = (await response.text())[:200]
                except Exception:
                    pass
                self._verification_resp = {"status": status, "body": body}
                if status >= 400:
                    self._log(f"http body: {body}", "WARN")
            elif status >= 400:
                try:
                    body = (await response.text())[:200]
                    self._log(f"http body: {body}", "WARN")
                except Exception:
                    pass
        except Exception:
            pass

    def _on_request_failed(self, req: Any) -> None:
        try:
            url = req.url
            if "blackbox.ai" not in url:
                return
            path = urlparse(url).path or "/"
            self._log(f"http {req.method} {path} FAILED ({req.failure or 'net'})", "WARN")
        except Exception:
            pass

    # ------------------------------------------------------------------
    # Full flow
    # ------------------------------------------------------------------

    async def register_and_create_key(
        self,
        email: str,
        password: str,
        poll_otp: Callable[[str, float], Awaitable[str]],
        on_step: Callable[[str], None] | None = None,
    ) -> str:
        """Run the entire verified flow and return the sk-... API key.

        poll_otp(email, window_seconds) polls the temp-mail worker for up to
        the given window and returns the code or "". This client drives the
        resend loop around it.
        """
        self._email = email
        page = self.page
        page.set_default_timeout(self._cfg.request_timeout * 1000)

        if on_step:
            on_step("signing up...")
        await self.signup(email, password)

        if on_step:
            on_step("waiting for otp...")
        code = await self._wait_otp_with_resend(email, poll_otp)
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

    async def login_with_google_and_create_key(
        self,
        email: str,
        password: str,
        on_step: Callable[[str], None] | None = None,
    ) -> str:
        """GSuite/Google SSO flow: login via "Continue with Google", harvest key.

        No OTP / temp-mail — the Google account password is the only input.
        Reuses the battle-tested Google-auth driver from google_sso.py
        (ported from qoder_farm) and the same create_api_key modal flow.
        """
        # Lazy import: google_sso is only needed for SSO mode and keeps the
        # register-mode dependency surface identical to before.
        from . import google_sso

        self._email = email
        page = self.page
        page.set_default_timeout(self._cfg.request_timeout * 1000)

        if on_step:
            on_step("opening login...")
        try:
            await page.goto(
                f"{self._cfg.blackbox_url}/login", wait_until="domcontentloaded"
            )
        except Exception as exc:
            await self._shot(email, "fail_sso_goto")
            raise BlackboxError(f"sso: goto /login failed: {exc}") from exc
        await asyncio.sleep(1.5)

        if on_step:
            on_step("google sign-in...")
        prog = _ProgressLogAdapter(self._log_fn)
        try:
            pages_before = set(self._context.pages) if self._context else set()
            await google_sso.click_blackbox_google_button(page, prog, email)
            auth_target = await self._detect_oauth_popup(pages_before) or page
            await google_sso.drive_google_auth(auth_target, email, password, prog)
        except Exception as exc:
            await self._shot(email, "fail_sso_auth")
            raise BlackboxError(f"sso: google auth failed: {exc}") from exc

        # After consent the app auto-logs-in. Poll for the SPA landing path
        # (same targets as verify_otp) instead of wait_for_url, which can
        # stall on 'load' for an SPA.
        if on_step:
            on_step("waiting for app...")
        deadline = asyncio.get_event_loop().time() + 60
        landed = False
        while asyncio.get_event_loop().time() < deadline:
            if re.search(r"/(activity|dashboard|chat)", page.url):
                landed = True
                break
            # Some accounts may land directly on root after SSO.
            try:
                parsed_path = urlparse(page.url).path.rstrip("/")
            except Exception:
                parsed_path = ""
            if google_sso.is_blackbox_host(page.url) and parsed_path in ("", "/"):
                landed = True
                break
            await asyncio.sleep(0.5)
        if not landed:
            await self._shot(email, "fail_sso_land")
            raise BlackboxError(
                f"sso: did not reach app after google login (still at {page.url})"
            )

        if on_step:
            on_step("creating api key...")
        api_key = await self.create_api_key()

        if on_step:
            on_step("done")
        return api_key

    async def _detect_oauth_popup(self, pages_before: set[Any]) -> Page | None:
        """Some Google SSO integrations open OAuth in a popup instead of
        redirecting the main tab. Returns the new popup page if one appeared,
        else None (caller falls back to driving the main page)."""
        if self._context is None:
            return None
        deadline = asyncio.get_event_loop().time() + 5
        while asyncio.get_event_loop().time() < deadline:
            for p in self._context.pages:
                if p in pages_before:
                    continue
                try:
                    url = p.url or ""
                except Exception:
                    url = ""
                if "accounts.google.com" in url or "google." in url:
                    return p
            await asyncio.sleep(0.3)
        return None

    async def _wait_otp_with_resend(
        self,
        email: str,
        poll_otp: Callable[[str, float], Awaitable[str]],
    ) -> str:
        # Live observation: the first verification email frequently never
        # arrives, but clicking "Resend" on the OTP screen triggers real
        # delivery. Blackbox rate-limits resend: 500 "Failed to send
        # verification code" intermittently, then 429 "Too many verification
        # code requests. Please try again in N minute(s)" — so parse the
        # cooldown and wait it out instead of spamming.
        loop = asyncio.get_event_loop()
        deadline = loop.time() + self._cfg.otp_timeout
        resend_n = 0

        code = await poll_otp(email, _FIRST_DELIVERY_WINDOW)
        while not code and loop.time() < deadline:
            resend_n += 1
            clicked = await self.click_resend()
            self._log(f"no OTP after window — resend #{resend_n} clicked={clicked}", "WARN")

            wait_s = 3.0
            resp = self._verification_resp
            status = resp.get("status", 0)
            body = resp.get("body", "") or ""
            if status == 429:
                m = re.search(r"in (\d+) minute", body)
                wait_s = min(240.0, (int(m.group(1)) * 60 + 10) if m else 190.0)
                self._log(f"resend rate-limited (429) — waiting {wait_s:.0f}s", "WARN")
            elif status == 500:
                wait_s = _500_RETRY_WAIT
                self._log(f"resend 500 (upstream send failed) — waiting {wait_s:.0f}s", "WARN")
            await asyncio.sleep(min(wait_s, max(0.0, deadline - loop.time())))

            remaining = deadline - loop.time()
            if remaining <= 0:
                break
            window = min(_AFTER_RESEND_WINDOW, max(10.0, remaining))
            code = await poll_otp(email, window)
        return code

    async def click_resend(self) -> bool:
        """Click the OTP screen's Resend control once it is clickable.

        The control shows "Sending..." while a send is in flight and "Resend"
        when clickable; clicking it mid-send is a no-op. Wait for the
        clickable state, then click. Returns True if clicked.
        """
        page = self.page
        deadline = asyncio.get_event_loop().time() + 60
        while asyncio.get_event_loop().time() < deadline:
            for sel in (
                'a:has-text("Resend")',
                'button:has-text("Resend")',
                'text=/Resend code/i',
                'text=/^Resend$/i',
            ):
                try:
                    loc = page.locator(sel).first
                    if await loc.count() > 0 and await loc.is_visible():
                        txt = (await loc.inner_text()).strip().lower()
                        if "sending" in txt:
                            continue
                        await loc.click()
                        return True
                except Exception:
                    continue
            await asyncio.sleep(1.0)
        return False

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
    # Step 3 — API key creation (with retries — intermittently flaky)
    # ------------------------------------------------------------------

    async def create_api_key(self, name: str | None = None, attempts: int = 4) -> str:
        key_name = name or self._cfg.key_name
        page = self.page

        # Listen for the key POST response in case the modal read-back fails.
        self._key_created = asyncio.Event()
        self._api_key = ""
        page.on(
            "response",
            lambda r: asyncio.create_task(self._capture_key_response(r)),
        )

        for attempt in range(1, attempts + 1):
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
                self._log(
                    f"create_key attempt {attempt}/{attempts} modal flow error: {str(exc)[:120]}",
                    "WARN",
                )
                await asyncio.sleep(2.5)
                continue

            # The key appears in a modal. Prefer reading it from the network
            # response, then fall back to scanning the page text.
            self._key_created = asyncio.Event()
            api_key = ""
            try:
                await asyncio.wait_for(self._key_created.wait(), timeout=15)
                api_key = self._api_key
            except asyncio.TimeoutError:
                pass

            if not api_key:
                api_key = await self._read_key_from_page()

            if api_key:
                await self._close_key_modal()
                return api_key

            # Manual testing shows creation intermittently returns nothing;
            # record the page state and retry.
            snippet = ""
            try:
                snippet = (await page.locator("body").inner_text())[:160].replace("\n", " ")
            except Exception:
                pass
            self._log(
                f"create_key attempt {attempt}/{attempts}: no key yet; page={snippet!r}",
                "WARN",
            )
            await asyncio.sleep(2.5)

        await self._shot(self._email, "fail_key_not_found")
        raise BlackboxError(
            f"create_key: API key not found after {attempts} attempts"
        )

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
