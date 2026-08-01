from __future__ import annotations

import unittest
from unittest.mock import AsyncMock, MagicMock, patch

from grok_farm.activate import _grok_signed_in, activate_grok_if_needed


def fake_page(*, selectors: set[str], body: str) -> AsyncMock:
    page = AsyncMock()

    def locate(selector: str) -> AsyncMock:
        locator = AsyncMock()
        locator.count = AsyncMock(return_value=int(selector in selectors))
        return locator

    page.locator = MagicMock(side_effect=locate)
    page.inner_text = AsyncMock(return_value=body)
    return page


class GrokSignedInTest(unittest.IsolatedAsyncioTestCase):
    async def test_current_authenticated_composer_is_recognized(self) -> None:
        page = fake_page(
            selectors={
                "[role='textbox'], text=/What's on your mind/i",
            },
            body="Grok What's on your mind? Fast",
        )

        self.assertTrue(await _grok_signed_in(page))

    async def test_signed_out_page_is_rejected(self) -> None:
        page = fake_page(
            selectors={
                "[role='textbox'], text=/What's on your mind/i",
            },
            body="Sign in Sign up What's on your mind?",
        )

        self.assertFalse(await _grok_signed_in(page))

    async def test_empty_page_is_rejected(self) -> None:
        self.assertFalse(await _grok_signed_in(fake_page(selectors=set(), body="")))


SIGNED_OUT_WITH_LEGAL_FOOTER = (
    "Sign in Sign up "
    "What's on your mind? Fast "
    "By messaging Grok, you agree to our Terms and Privacy Policy. "
    "Essential cookies are always active. Accept Continue"
)


class ActivateSignedOutLegalFooterTest(unittest.IsolatedAsyncioTestCase):
    @patch("grok_farm.activate._sso_handoff_to_grok", new_callable=AsyncMock)
    @patch("grok_farm.activate._grok_signed_in", new_callable=AsyncMock)
    @patch("grok_farm.activate.click_text_button", new_callable=AsyncMock)
    @patch("grok_farm.activate.dismiss_cookie_banner", new_callable=AsyncMock)
    @patch("grok_farm.activate.asyncio.sleep", new_callable=AsyncMock)
    async def test_signed_out_with_footer_uses_sso_not_terms(
        self,
        mock_sleep: AsyncMock,
        mock_dismiss: AsyncMock,
        mock_click: AsyncMock,
        mock_signed_in: AsyncMock,
        mock_sso: AsyncMock,
    ) -> None:
        mock_signed_in.return_value = False
        mock_click.return_value = False

        page = AsyncMock()
        page.inner_text = AsyncMock(return_value=SIGNED_OUT_WITH_LEGAL_FOOTER)

        cfg = MagicMock()
        prog = MagicMock()

        result = await activate_grok_if_needed(page, cfg, prog, "a@b.com")

        self.assertFalse(result)
        mock_sso.assert_awaited()
        mock_click.assert_not_awaited()


if __name__ == "__main__":
    unittest.main()
