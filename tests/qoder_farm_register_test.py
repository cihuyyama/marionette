from __future__ import annotations

import base64
import io
import unittest

from qoder_farm.captcha import detect_gap_offset
from qoder_farm.email_signup import generate_email
from qoder_farm.eligibility import inject_eligibility
from qoder_farm.imap import extract_code, matches_target

try:
    import numpy as np
    from PIL import Image

    _HAVE_CV = True
except Exception:
    _HAVE_CV = False


def _data_uri(img: "Image.Image") -> str:
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()


def _make_puzzle(gap_left: int, piece_w: int = 49, w: int = 296, h: int = 200):
    # Smooth horizontal gradient background so the only strong vertical edges are
    # the notch borders (faithful to real puzzles; random noise would add spurious
    # edges and defeat the paired-peak detector).
    xs = np.linspace(40, 210, w, dtype=np.uint8)
    bg = np.repeat(xs[None, :, None], h, axis=0)
    bg = np.repeat(bg, 3, axis=2)
    r0, r1 = 60, 140
    notch = bg.copy()
    notch[r0:r1, gap_left : gap_left + piece_w] = 20
    back = Image.fromarray(notch, "RGB").convert("RGBA")

    piece = np.zeros((h, w, 4), dtype=np.uint8)
    piece[r0:r1, 2 : 2 + piece_w, :3] = 120
    piece[r0:r1, 2 : 2 + piece_w, 3] = 255
    return _data_uri(back), _data_uri(Image.fromarray(piece, "RGBA"))


class EmailGenTest(unittest.TestCase):
    def test_catch_all_domain_uses_random_local(self) -> None:
        email = generate_email("example.com")
        self.assertTrue(email.endswith("@example.com"))
        self.assertNotIn("+", email)

    def test_gmail_base_uses_plus_tag(self) -> None:
        email = generate_email("me@gmail.com")
        self.assertTrue(email.startswith("me+"))
        self.assertTrue(email.endswith("@gmail.com"))

    def test_generated_emails_unique(self) -> None:
        seen = {generate_email("d.com") for _ in range(50)}
        self.assertEqual(len(seen), 50)


class OtpExtractTest(unittest.TestCase):
    def test_code_in_subject(self) -> None:
        self.assertEqual(extract_code("Your Qoder code: 482913", ""), "482913")

    def test_code_in_html_body(self) -> None:
        self.assertEqual(extract_code("verify", "<b>001234</b> is your code"), "001234")

    def test_no_code_returns_none(self) -> None:
        self.assertIsNone(extract_code("welcome", "no digits of length six here 12 345"))

    def test_style_noise_ignored_before_real_code(self) -> None:
        body = "<style>.x{width:123px}</style> code 654321"
        self.assertEqual(extract_code("hi", body), "654321")


class InjectEligibilityTest(unittest.TestCase):
    def test_fresh_community_account_is_eligible(self) -> None:
        self.assertEqual(
            inject_eligibility({"quotaLimit": 0, "quotaRemaining": 0, "plan": "Community"}),
            (True, "fresh free account"),
        )

    def test_exhausted_credit_bucket_is_ineligible(self) -> None:
        eligible, reason = inject_eligibility(
            {"quotaLimit": 300, "quotaRemaining": 0, "plan": "Community"}
        )
        self.assertFalse(eligible)
        self.assertIn("prior credit bucket", reason)

    def test_active_credit_bucket_is_ineligible(self) -> None:
        self.assertFalse(
            inject_eligibility({"quotaLimit": 300, "quotaRemaining": 150, "plan": "Community"})[0]
        )

    def test_pro_trial_is_ineligible_even_with_zero_limit(self) -> None:
        eligible, reason = inject_eligibility(
            {"quotaLimit": 0, "quotaRemaining": 0, "plan": "Pro Trial"}
        )
        self.assertFalse(eligible)
        self.assertIn("plan", reason)

    def test_missing_quota_is_fail_closed(self) -> None:
        eligible, reason = inject_eligibility(None)
        self.assertFalse(eligible)
        self.assertIn("unverifiable", reason)

    def test_incomplete_quota_is_fail_closed(self) -> None:
        eligible, reason = inject_eligibility({"plan": "Community"})
        self.assertFalse(eligible)
        self.assertIn("unverifiable", reason)

    def test_missing_plan_is_fail_closed(self) -> None:
        eligible, reason = inject_eligibility({"quotaLimit": 0, "quotaRemaining": 0})
        self.assertFalse(eligible)
        self.assertIn("unverifiable", reason)

    def test_unparseable_quota_limit_is_fail_closed(self) -> None:
        eligible, reason = inject_eligibility(
            {"quotaLimit": "unknown", "quotaRemaining": 0, "plan": "Community"}
        )
        self.assertFalse(eligible)
        self.assertIn("unverifiable", reason)


class MatchesTargetTest(unittest.TestCase):
    def test_empty_target_matches_any(self) -> None:
        self.assertTrue(matches_target({"To": "anyone@x.com"}, ""))

    def test_delivered_to_header(self) -> None:
        msg = {"Delivered-To": "abc+tag@gmail.com"}
        self.assertTrue(matches_target(msg, "abc+tag@gmail.com"))

    def test_non_match(self) -> None:
        self.assertFalse(matches_target({"To": "other@x.com"}, "target@x.com"))


@unittest.skipUnless(_HAVE_CV, "numpy/Pillow required")
class CaptchaGapTest(unittest.TestCase):
    def test_detects_gap_offset_in_track_range(self) -> None:
        back, piece = _make_puzzle(gap_left=150)
        offset = detect_gap_offset(back, piece, puzzle_disp_w=300, track_w=300, handle_w=40)
        self.assertIsNotNone(offset)
        self.assertGreater(offset, 0)
        self.assertLess(offset, 260)

    def test_further_gap_gives_larger_offset(self) -> None:
        near = detect_gap_offset(*_make_puzzle(gap_left=90), puzzle_disp_w=300, track_w=300, handle_w=40)
        far = detect_gap_offset(*_make_puzzle(gap_left=210), puzzle_disp_w=300, track_w=300, handle_w=40)
        self.assertLess(near, far)


if __name__ == "__main__":
    unittest.main()
