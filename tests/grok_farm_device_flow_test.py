from __future__ import annotations

import unittest
from unittest.mock import AsyncMock, MagicMock, patch

from grok_farm.device_flow import (
    _device_page_state,
    _drive_device_confirmation,
    _validate_device_tokens,
)


def fake_page(*, url: str = "", selectors: set[str] | None = None, body: str = "") -> AsyncMock:
    selectors = selectors or set()
    page = AsyncMock()
    page.url = url

    def locate(selector: str) -> AsyncMock:
        locator = AsyncMock()
        locator.count = AsyncMock(return_value=int(selector in selectors))
        locator.first = locator
        locator.is_visible = AsyncMock(return_value=True)
        locator.inner_text = AsyncMock(return_value="")
        return locator

    page.locator = MagicMock(side_effect=locate)
    page.inner_text = AsyncMock(return_value=body)
    page.evaluate = AsyncMock(return_value=body)
    return page


class DevicePageStateTest(unittest.IsolatedAsyncioTestCase):
    async def test_done_url_classified_as_done(self) -> None:
        page = fake_page(url="https://accounts.x.ai/oauth2/device/done")
        self.assertEqual(await _device_page_state(page), "done")

    async def test_approved_url_classified_as_done(self) -> None:
        page = fake_page(url="https://accounts.x.ai/oauth2/device/approved")
        self.assertEqual(await _device_page_state(page), "done")

    async def test_login_form_classified_as_login(self) -> None:
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={'input[type="password"]', 'input[type="email"]'},
            body="Log in with your email",
        )
        self.assertEqual(await _device_page_state(page), "login")

    async def test_consent_page_classified_as_consent(self) -> None:
        page = fake_page(
            url="https://accounts.x.ai/oauth2/consent",
            body="Authorize Grok to access your account Allow Continue",
        )
        self.assertEqual(await _device_page_state(page), "consent")

    async def test_denied_page_classified_as_error(self) -> None:
        page = fake_page(
            url="https://accounts.x.ai/oauth2/device/verify",
            body="Access denied. Authorization failed.",
        )
        self.assertEqual(await _device_page_state(page), "error")

    async def test_unknown_page_classified_as_unknown(self) -> None:
        page = fake_page(url="https://accounts.x.ai/some-other-page", body="Loading...")
        self.assertEqual(await _device_page_state(page), "unknown")

    async def test_token_state_takes_priority_over_done_url(self) -> None:
        page = fake_page(url="https://accounts.x.ai/oauth2/device/done")
        self.assertEqual(await _device_page_state(page, token_ready=True), "token")


class DriveDeviceConfirmationLoginTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.device_flow.drive_email_password_login", new_callable=AsyncMock)
    async def test_login_state_drives_email_password_login(self, mock_login: AsyncMock) -> None:
        mock_login.return_value = True
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={'input[type="password"]'},
        )
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page, "login", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        self.assertTrue(result)
        mock_login.assert_awaited_once()

    @patch("grok_farm.device_flow.drive_email_password_login", new_callable=AsyncMock)
    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    async def test_login_state_does_not_use_generic_continue(
        self, mock_click: AsyncMock, mock_login: AsyncMock
    ) -> None:
        mock_login.return_value = True
        page = fake_page(url="https://accounts.x.ai/sign-in")
        prog = MagicMock()

        await _drive_device_confirmation(
            page, "login", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        mock_login.assert_awaited_once()
        if mock_click.await_count > 0:
            for call in mock_click.await_args_list:
                keywords = call[0][1] if len(call[0]) > 1 else call[1].get("keywords", [])
                self.assertNotIn("Continue", keywords)


class DriveDeviceConfirmationConsentTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    async def test_consent_prefers_allow_over_continue(self, mock_click: AsyncMock) -> None:
        mock_click.return_value = "Allow"
        page = fake_page(url="https://accounts.x.ai/oauth2/consent")
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page, "consent", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        self.assertTrue(result)
        mock_click.assert_awaited()
        first_call = mock_click.await_args_list[0]
        keywords = first_call[0][1] if len(first_call[0]) > 1 else first_call[1].get("keywords", [])
        allow_idx = keywords.index("Allow") if "Allow" in keywords else 999
        continue_idx = keywords.index("Continue") if "Continue" in keywords else 999
        self.assertLess(allow_idx, continue_idx)

    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    @patch("grok_farm.device_flow._consent_force_allow", new_callable=AsyncMock)
    async def test_consent_falls_back_to_force_allow(
        self, mock_force: AsyncMock, mock_click: AsyncMock
    ) -> None:
        mock_click.return_value = None
        mock_force.return_value = True
        page = fake_page(url="https://accounts.x.ai/oauth2/consent")
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page, "consent", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        self.assertTrue(result)
        mock_force.assert_awaited_once()


class DriveDeviceConfirmationDoneTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    async def test_done_state_performs_no_click(self, mock_click: AsyncMock) -> None:
        page = fake_page(url="https://accounts.x.ai/oauth2/device/done")
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page, "done", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        self.assertTrue(result)
        mock_click.assert_not_awaited()


class DriveDeviceConfirmationDeniedTest(unittest.IsolatedAsyncioTestCase):
    async def test_error_state_fails_immediately(self) -> None:
        page = fake_page(
            url="https://accounts.x.ai/oauth2/device/verify",
            body="Access denied",
        )
        prog = MagicMock()

        with self.assertRaises(RuntimeError):
            await _drive_device_confirmation(
                page, "error", email="a@b.com", password="pw", prog=prog, label="a@b.com"
            )


class ValidateDeviceTokensTest(unittest.TestCase):
    def test_access_and_refresh_accepted(self) -> None:
        tokens = {"access_token": "at_123", "refresh_token": "rt_456"}
        self.assertTrue(_validate_device_tokens(tokens))

    def test_access_without_refresh_rejected(self) -> None:
        tokens = {"access_token": "at_123", "refresh_token": ""}
        self.assertFalse(_validate_device_tokens(tokens))

    def test_missing_refresh_key_rejected(self) -> None:
        tokens = {"access_token": "at_123"}
        self.assertFalse(_validate_device_tokens(tokens))

    def test_empty_dict_rejected(self) -> None:
        self.assertFalse(_validate_device_tokens({}))

    def test_none_rejected(self) -> None:
        self.assertFalse(_validate_device_tokens(None))


class DevicePageStateEmailChoiceTest(unittest.IsolatedAsyncioTestCase):
    async def test_email_choice_button_classified_as_login(self) -> None:
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={"text=/Continue with email|Sign in with email|Login with email|Log in with email/i"},
            body="Choose how to sign in",
        )
        self.assertEqual(await _device_page_state(page), "login")

    async def test_email_only_input_classified_as_login(self) -> None:
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={'input[type="email"], input[name="email"]'},
            body="Enter your email",
        )
        self.assertEqual(await _device_page_state(page), "login")


class DriveDeviceConfirmationEmailChoiceTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.device_flow.drive_email_password_login", new_callable=AsyncMock)
    @patch("grok_farm.device_flow.click_login_with_email", new_callable=AsyncMock)
    async def test_login_clicks_email_choice_before_drive(
        self, mock_email_choice: AsyncMock, mock_login: AsyncMock
    ) -> None:
        mock_email_choice.return_value = True
        mock_login.return_value = True
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={"text=/Continue with email|Sign in with email|Login with email|Log in with email/i"},
        )
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page, "login", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        self.assertTrue(result)
        mock_email_choice.assert_awaited_once()
        mock_login.assert_awaited_once()

    @patch("grok_farm.device_flow.drive_email_password_login", new_callable=AsyncMock)
    @patch("grok_farm.device_flow.click_login_with_email", new_callable=AsyncMock)
    async def test_login_skips_email_choice_when_password_visible(
        self, mock_email_choice: AsyncMock, mock_login: AsyncMock
    ) -> None:
        mock_login.return_value = True
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={
                'input[type="password"]',
                "text=/Continue with email|Sign in with email|Login with email|Log in with email/i",
            },
        )
        prog = MagicMock()

        await _drive_device_confirmation(
            page, "login", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        mock_email_choice.assert_not_awaited()
        mock_login.assert_awaited_once()


class ObtainTokensPasswordIntegrationTest(unittest.TestCase):
    def test_register_passes_password_to_device_flow(self) -> None:
        import inspect
        import grok_farm.register as reg

        source = inspect.getsource(reg.register_one)
        self.assertIn("password=password", source)


class DriveDeviceConfirmationOtpTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.device_flow.handle_optional_otp", new_callable=AsyncMock)
    @patch("grok_farm.device_flow.drive_email_password_login", new_callable=AsyncMock)
    async def test_login_invokes_otp_when_cfg_provided(
        self, mock_login: AsyncMock, mock_otp: AsyncMock
    ) -> None:
        mock_login.return_value = True
        mock_otp.return_value = None
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={'input[type="password"]'},
        )
        prog = MagicMock()
        cfg = MagicMock()

        result = await _drive_device_confirmation(
            page, "login", email="a@b.com", password="pw", prog=prog, label="a@b.com", cfg=cfg
        )

        self.assertTrue(result)
        mock_login.assert_awaited_once()
        mock_otp.assert_awaited_once_with(page, "a@b.com", cfg, prog, "a@b.com")

    @patch("grok_farm.device_flow.handle_optional_otp", new_callable=AsyncMock)
    @patch("grok_farm.device_flow.drive_email_password_login", new_callable=AsyncMock)
    async def test_login_skips_otp_when_cfg_is_none(
        self, mock_login: AsyncMock, mock_otp: AsyncMock
    ) -> None:
        mock_login.return_value = True
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={'input[type="password"]'},
        )
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page, "login", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        self.assertTrue(result)
        mock_otp.assert_not_awaited()

    @patch("grok_farm.device_flow.handle_optional_otp", new_callable=AsyncMock)
    @patch("grok_farm.device_flow.drive_email_password_login", new_callable=AsyncMock)
    async def test_otp_failure_propagates(
        self, mock_login: AsyncMock, mock_otp: AsyncMock
    ) -> None:
        mock_login.return_value = True
        mock_otp.side_effect = RuntimeError("OTP required but not completed")
        page = fake_page(
            url="https://accounts.x.ai/sign-in",
            selectors={'input[type="password"]'},
        )
        prog = MagicMock()
        cfg = MagicMock()

        with self.assertRaises(RuntimeError):
            await _drive_device_confirmation(
                page, "login", email="a@b.com", password="pw", prog=prog, label="a@b.com", cfg=cfg
            )


SENTINEL_USER_CODE = "WDJB-MJHT"
SENTINEL_DEVICE_CODE = "dc_sentinel_abc123"


def fake_device_code_page(
    *,
    url: str = "https://accounts.x.ai/oauth2/device",
    input_value: str = "",
    has_user_code_input: bool = True,
    body: str = "Enter your code",
) -> AsyncMock:
    page = AsyncMock()
    page.url = url
    _locators: dict[str, AsyncMock] = {}

    def locate(selector: str) -> AsyncMock:
        if selector in _locators:
            return _locators[selector]
        locator = AsyncMock()
        if 'input[name="user_code"]' in selector and has_user_code_input:
            locator.count = AsyncMock(return_value=1)
        else:
            locator.count = AsyncMock(return_value=0)
        locator.first = locator
        locator.is_visible = AsyncMock(return_value=True)
        locator.inner_text = AsyncMock(return_value="")
        locator.input_value = AsyncMock(return_value=input_value)
        locator.fill = AsyncMock()
        locator.click = AsyncMock()
        _locators[selector] = locator
        return locator

    page.locator = MagicMock(side_effect=locate)
    page.inner_text = AsyncMock(return_value=body)
    page.evaluate = AsyncMock(return_value=body)
    return page


class DevicePageStateDeviceCodeTest(unittest.IsolatedAsyncioTestCase):
    async def test_device_url_classified_as_device_code(self) -> None:
        page = fake_device_code_page(url="https://accounts.x.ai/oauth2/device")
        self.assertEqual(await _device_page_state(page), "device_code")

    async def test_user_code_input_classified_as_device_code(self) -> None:
        page = fake_device_code_page(
            url="https://accounts.x.ai/some-other-path",
            has_user_code_input=True,
        )
        self.assertEqual(await _device_page_state(page), "device_code")

    async def test_token_still_takes_priority_over_device_code(self) -> None:
        page = fake_device_code_page(url="https://accounts.x.ai/oauth2/device")
        self.assertEqual(await _device_page_state(page, token_ready=True), "token")

    async def test_done_still_takes_priority_over_device_code(self) -> None:
        page = fake_device_code_page(url="https://accounts.x.ai/oauth2/device/done")
        self.assertEqual(await _device_page_state(page), "done")

    async def test_error_still_takes_priority_over_device_code(self) -> None:
        page = fake_device_code_page(
            url="https://accounts.x.ai/oauth2/device",
            body="Access denied. Authorization failed.",
        )
        self.assertEqual(await _device_page_state(page), "error")

    async def test_login_still_takes_priority_over_device_code(self) -> None:
        page = fake_page(
            url="https://accounts.x.ai/oauth2/device",
            selectors={'input[type="password"]', 'input[name="user_code"]'},
            body="Enter code",
        )
        self.assertEqual(await _device_page_state(page), "login")

    async def test_consent_still_takes_priority_over_device_code(self) -> None:
        page = fake_page(
            url="https://accounts.x.ai/oauth2/consent",
            body="Authorize Grok to access your account Allow",
        )
        self.assertEqual(await _device_page_state(page), "consent")


class DriveDeviceConfirmationDeviceCodeTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    async def test_fills_empty_input_and_clicks_continue(self, mock_click: AsyncMock) -> None:
        mock_click.return_value = "Continue"
        page = fake_device_code_page(input_value="")
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page,
            "device_code",
            email="a@b.com",
            password="pw",
            prog=prog,
            label="a@b.com",
            user_code=SENTINEL_USER_CODE,
        )

        self.assertTrue(result)
        locator = page.locator('input[name="user_code"]')
        locator.fill.assert_awaited_once_with(SENTINEL_USER_CODE)
        mock_click.assert_awaited()
        first_call = mock_click.await_args_list[0]
        keywords = first_call[0][1] if len(first_call[0]) > 1 else first_call[1].get("keywords", [])
        self.assertIn("Continue", keywords)

    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    async def test_prefilled_input_clicks_continue_without_rewrite(
        self, mock_click: AsyncMock
    ) -> None:
        mock_click.return_value = "Continue"
        page = fake_device_code_page(input_value=SENTINEL_USER_CODE)
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page,
            "device_code",
            email="a@b.com",
            password="pw",
            prog=prog,
            label="a@b.com",
            user_code=SENTINEL_USER_CODE,
        )

        self.assertTrue(result)
        locator = page.locator('input[name="user_code"]')
        locator.fill.assert_not_awaited()
        mock_click.assert_awaited()

    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    async def test_no_input_still_clicks_continue(self, mock_click: AsyncMock) -> None:
        mock_click.return_value = "Continue"
        page = fake_device_code_page(has_user_code_input=False)
        prog = MagicMock()

        result = await _drive_device_confirmation(
            page,
            "device_code",
            email="a@b.com",
            password="pw",
            prog=prog,
            label="a@b.com",
            user_code=SENTINEL_USER_CODE,
        )

        self.assertTrue(result)
        mock_click.assert_awaited()


class DriveDeviceConfirmationConsentNoContinueTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    async def test_consent_never_includes_continue_keyword(self, mock_click: AsyncMock) -> None:
        mock_click.return_value = "Allow"
        page = fake_page(url="https://accounts.x.ai/oauth2/consent")
        prog = MagicMock()

        await _drive_device_confirmation(
            page, "consent", email="a@b.com", password="pw", prog=prog, label="a@b.com"
        )

        mock_click.assert_awaited()
        for call in mock_click.await_args_list:
            keywords = call[0][1] if len(call[0]) > 1 else call[1].get("keywords", [])
            self.assertNotIn("Continue", keywords)


class DeviceCodeLoggingSafetyTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.device_flow.click_text_button", new_callable=AsyncMock)
    async def test_user_code_never_logged(self, mock_click: AsyncMock) -> None:
        mock_click.return_value = "Continue"
        page = fake_device_code_page(input_value="")
        prog = MagicMock()

        await _drive_device_confirmation(
            page,
            "device_code",
            email="a@b.com",
            password="pw",
            prog=prog,
            label="a@b.com",
            user_code=SENTINEL_USER_CODE,
        )

        for call in prog.log.call_args_list:
            msg = call[0][0] if call[0] else str(call)
            self.assertNotIn(SENTINEL_USER_CODE, str(msg))
            self.assertNotIn(SENTINEL_DEVICE_CODE, str(msg))


if __name__ == "__main__":
    unittest.main()
