from __future__ import annotations

import asyncio
import base64
import io
import math
import random
import time
import urllib.request
from typing import Any

from .progress import Progress

try:
    import numpy as np
    from PIL import Image

    _HAVE_CV = True
except Exception:
    _HAVE_CV = False


REFRESH_LABEL = "刷新验证码"


def _fetch_image(src: str) -> "Image.Image":
    if src.startswith("data:"):
        raw = base64.b64decode(src.split(",", 1)[1])
    else:
        raw = urllib.request.urlopen(src, timeout=20).read()
    return Image.open(io.BytesIO(raw)).convert("RGBA")


def _sobel_x(gray: "np.ndarray") -> "np.ndarray":
    """Absolute horizontal Sobel gradient (vertical edges) via numpy, no cv2."""
    g = gray.astype(np.float64)
    # separable Sobel-x: smooth in y [1,2,1], diff in x [-1,0,1]
    sy = np.zeros_like(g)
    sy[1:-1, :] = g[:-2, :] + 2.0 * g[1:-1, :] + g[2:, :]
    sy[0, :] = 3.0 * g[0, :]
    sy[-1, :] = 3.0 * g[-1, :]
    gx = np.zeros_like(g)
    gx[:, 1:-1] = sy[:, 2:] - sy[:, :-2]
    gx[:, 0] = sy[:, 1] - sy[:, 0]
    gx[:, -1] = sy[:, -1] - sy[:, -2]
    return np.abs(gx)


def _ncc_1d(band: "np.ndarray", template: "np.ndarray", x_lo: int, x_hi: int) -> tuple[int, float]:
    """Normalized cross-correlation of `template` slid horizontally across `band`.

    Both arrays share the same height (the piece y-band). Returns (best_x, score)
    where score is Pearson correlation (TM_CCOEFF_NORMED equivalent).
    """
    pw = template.shape[1]
    t = template - template.mean()
    t_norm = math.sqrt(float((t * t).sum())) or 1.0
    best_x, best_s = -1, -2.0
    for x in range(x_lo, x_hi):
        win = band[:, x : x + pw]
        w = win - win.mean()
        w_norm = math.sqrt(float((w * w).sum())) or 1.0
        s = float((w * t).sum()) / (w_norm * t_norm)
        if s > best_s:
            best_s, best_x = s, x
    return best_x, best_s


def detect_gap_offset(
    back_src: str,
    piece_src: str,
    puzzle_disp_w: float,
    track_w: float,
    handle_w: float,
) -> float | None:
    """Return the slider-handle drag distance (displayed px) that aligns the
    puzzle piece with the notch.

    Gap detection (ported from the proven cv2 Aliyun solver to pure numpy):
    build a template = Sobel-x of the piece silhouette (from the shadow alpha),
    then slide it across the Sobel-x of the back image restricted to the piece's
    y-band. The gap lies strictly right of the piece home, so that region is
    masked out before argmax. Best-scoring x = gap left edge.

    Images are fetched in Python because Aliyun serves them cross-origin, which
    taints the page canvas and blocks getImageData in the browser.

    Handle-vs-piece travel differ (handle spans track-handle, piece spans
    puzzle-piece), so the piece offset is scaled by that ratio.
    """
    if not _HAVE_CV:
        return None
    try:
        back = _fetch_image(back_src)
        piece = _fetch_image(piece_src)
    except Exception:
        return None

    pa = np.array(piece)
    alpha = pa[:, :, 3]
    cols = np.where(alpha.max(axis=0) > 30)[0]
    rows = np.where(alpha.max(axis=1) > 30)[0]
    if not len(cols) or not len(rows):
        return None
    piece_home = int(cols.min())
    piece_w = int(cols.max() - cols.min() + 1)
    r0, r1 = int(rows.min()), int(rows.max())

    g = np.array(back.convert("L"), dtype=np.float64)
    nat_w = g.shape[1]
    if nat_w <= piece_w + 4:
        return None

    back_gx = _sobel_x(g)[r0 : r1 + 1, :]
    silhouette = (alpha[r0 : r1 + 1, piece_home : piece_home + piece_w] > 30).astype(np.float64) * 255.0
    # pad 1px so a near-rectangular piece still yields left+right border edges
    # (piece_w apart), matching the gap's two borders; jigsaw contours add more.
    silhouette = np.pad(silhouette, ((0, 0), (1, 1)))
    template_gx = _sobel_x(silhouette)

    # gap is strictly right of the piece home; keep a small margin off the far
    # edge where AIGC backgrounds throw false vertical edges (seen locking x_hi).
    x_lo = piece_home + piece_w
    x_hi = nat_w - template_gx.shape[1] - 3
    if x_lo >= x_hi:
        return None
    best_x, best_s = _ncc_1d(back_gx, template_gx, x_lo, x_hi)
    if best_x < 0:
        return None

    img_scale = puzzle_disp_w / nat_w
    piece_home_disp = piece_home * img_scale
    gap_disp = (best_x + 1) * img_scale
    piece_travel = gap_disp - piece_home_disp

    piece_disp_w = piece_w * img_scale
    handle_span = max(1.0, track_w - handle_w)
    piece_span = max(1.0, puzzle_disp_w - piece_disp_w)
    map_ratio = handle_span / piece_span
    return piece_travel * map_ratio


async def _read_captcha_geometry(page: Any) -> dict[str, Any] | None:
    return await page.evaluate(
        """() => {
            const puzzle = document.getElementById('aliyunCaptcha-img')
                || [...document.querySelectorAll('img')].find(i => i.className === 'puzzle');
            const piece = document.getElementById('aliyunCaptcha-puzzle')
                || [...document.querySelectorAll('img')].find(i => i.naturalWidth <= 60 && i.naturalHeight === 200);
            const handle = document.getElementById('aliyunCaptcha-sliding-slider')
                || document.querySelector('.slider-move');
            if (!puzzle || !piece || !handle) return null;
            const track = handle.parentElement;
            const hb = handle.getBoundingClientRect();
            const tb = track.getBoundingClientRect();
            const pb = puzzle.getBoundingClientRect();
            return {
                back: puzzle.src,
                piece: piece.src,
                puzzleDispW: pb.width,
                trackW: tb.width,
                handleW: hb.width,
                handleX: hb.x + hb.width / 2,
                handleY: hb.y + hb.height / 2,
            };
        }"""
    )


_DETECT_JS = r"""async () => {
    const bgImg = document.getElementById('aliyunCaptcha-img');
    const pzImg = document.getElementById('aliyunCaptcha-puzzle');
    const slider = document.getElementById('aliyunCaptcha-sliding-slider');
    if (!bgImg || !pzImg || !slider) return null;
    const track = slider.parentElement;
    const getPixels = async (img) => {
        const res = await fetch(img.src, { signal: AbortSignal.timeout(10000) });
        const blob = await res.blob();
        const url = URL.createObjectURL(blob);
        const tmp = new Image();
        await new Promise((r, j) => { tmp.onload = r; tmp.onerror = j; tmp.src = url; });
        const c = document.createElement('canvas');
        c.width = tmp.naturalWidth; c.height = tmp.naturalHeight;
        c.getContext('2d').drawImage(tmp, 0, 0);
        const id = c.getContext('2d').getImageData(0, 0, c.width, c.height);
        URL.revokeObjectURL(url);
        return { w: c.width, h: c.height, data: id.data };
    };
    let bg, pz;
    try { bg = await getPixels(bgImg); pz = await getPixels(pzImg); }
    catch (e) { return { error: 'pixels:' + e.message }; }

    let pzMinX = pz.w, pzMaxX = 0, pzMinY = pz.h, pzMaxY = 0;
    for (let y = 0; y < pz.h; y++) for (let x = 0; x < pz.w; x++) {
        if (pz.data[(y * pz.w + x) * 4 + 3] > 128) {
            if (x < pzMinX) pzMinX = x; if (x > pzMaxX) pzMaxX = x;
            if (y < pzMinY) pzMinY = y; if (y > pzMaxY) pzMaxY = y;
        }
    }
    const cropW = pzMaxX - pzMinX + 1, cropH = pzMaxY - pzMinY + 1;
    const cropN = cropW * cropH;
    const pzGray = new Float64Array(cropN), pzMask = new Uint8Array(cropN);
    let maskCount = 0;
    for (let cy = 0; cy < cropH; cy++) for (let cx = 0; cx < cropW; cx++) {
        const s = ((pzMinY + cy) * pz.w + (pzMinX + cx)) * 4, d = cy * cropW + cx;
        const a = pz.data[s + 3]; pzMask[d] = a > 128 ? 1 : 0; if (a > 128) maskCount++;
        pzGray[d] = pz.data[s] * 0.299 + pz.data[s + 1] * 0.587 + pz.data[s + 2] * 0.114;
    }
    const bgN = bg.w * bg.h, bgGray = new Float64Array(bgN);
    for (let i = 0; i < bgN; i++) { const k = i * 4;
        bgGray[i] = bg.data[k] * 0.299 + bg.data[k + 1] * 0.587 + bg.data[k + 2] * 0.114; }
    const blur = (data, w, h) => {
        const o = new Float64Array(w * h);
        for (let y = 1; y < h - 1; y++) for (let x = 1; x < w - 1; x++)
            o[y*w+x] = (data[(y-1)*w+(x-1)]+2*data[(y-1)*w+x]+data[(y-1)*w+(x+1)]
                +2*data[y*w+(x-1)]+4*data[y*w+x]+2*data[y*w+(x+1)]
                +data[(y+1)*w+(x-1)]+2*data[(y+1)*w+x]+data[(y+1)*w+(x+1)]) / 16;
        return o;
    };
    const bgB = blur(bgGray, bg.w, bg.h), pzB = blur(pzGray, cropW, cropH);
    const maskedNCC = (src, sw, sh, tpl, tw, th, mask, mN, offY) => {
        let bx = -1, by = -1, bs = -2;
        const y0 = Math.max(0, offY - 10), y1 = Math.min(sh - th, offY + 10);
        for (let ox = 1; ox <= sw - tw - 1; ox++) for (let oy = y0; oy <= y1; oy++) {
            let tm = 0, sm = 0;
            for (let i = 0; i < mN; i++) { if (!mask[i]) continue;
                const tx = i % tw, ty = (i / tw) | 0; tm += tpl[i]; sm += src[(oy+ty)*sw+(ox+tx)]; }
            tm /= mN; sm /= mN;
            let tv = 0, sv = 0, cv = 0;
            for (let i = 0; i < mN; i++) { if (!mask[i]) continue;
                const tx = i % tw, ty = (i / tw) | 0;
                const td = tpl[i] - tm, sd = src[(oy+ty)*sw+(ox+tx)] - sm;
                tv += td*td; sv += sd*sd; cv += sd*td; }
            const ts = Math.sqrt(tv/mN), ss = Math.sqrt(sv/mN);
            if (ts < 0.001 || ss < 0.001) continue;
            const ncc = (cv/mN) / (ss*ts);
            if (ncc > bs) { bs = ncc; bx = ox; by = oy; }
        }
        return { bx, by, bs };
    };
    const canny = (data, w, h) => {
        const e = new Float64Array(w * h);
        for (let y = 1; y < h - 1; y++) for (let x = 1; x < w - 1; x++) {
            const gx = -data[(y-1)*w+(x-1)]+data[(y-1)*w+(x+1)]-2*data[y*w+(x-1)]
                +2*data[y*w+(x+1)]-data[(y+1)*w+(x-1)]+data[(y+1)*w+(x+1)];
            const gy = -data[(y-1)*w+(x-1)]-2*data[(y-1)*w+x]-data[(y-1)*w+(x+1)]
                +data[(y+1)*w+(x-1)]+2*data[(y+1)*w+x]+data[(y+1)*w+(x+1)];
            const m = Math.sqrt(gx*gx+gy*gy); e[y*w+x] = m > 150 ? 255 : (m > 50 ? 128 : 0);
        }
        return e;
    };
    const grayR = maskedNCC(bgB, bg.w, bg.h, pzB, cropW, cropH, pzMask, maskCount, pzMinY);
    const edgeR = maskedNCC(canny(bgB, bg.w, bg.h), bg.w, bg.h,
        canny(pzB, cropW, cropH), cropW, cropH, pzMask, maskCount, pzMinY);

    const minValidX = 15;
    const cands = [];
    if (grayR.bx >= minValidX) cands.push({ x: grayR.bx, score: grayR.bs, m: 'gray' });
    if (edgeR.bx >= minValidX) cands.push({ x: edgeR.bx, score: edgeR.bs, m: 'edge' });
    let finalX = -1, finalScore = 0, method = '';
    for (let i = 0; i < cands.length; i++) for (let j = i + 1; j < cands.length; j++)
        if (Math.abs(cands[i].x - cands[j].x) < 15) {
            const avg = (cands[i].score + cands[j].score) / 2;
            if (avg > finalScore) { finalX = Math.round((cands[i].x+cands[j].x)/2);
                finalScore = avg; method = cands[i].m + '+' + cands[j].m; }
        }
    if (finalX < 0) {
        const g = cands.find(c => c.m === 'gray');
        if (g && g.score >= 0.4) { finalX = g.x; finalScore = g.score; method = 'gray'; }
        else if (cands.length) { let b = cands[0];
            for (const c of cands) if (c.score > b.score) b = c;
            finalX = b.x; finalScore = b.score; method = b.m + '?'; }
    }
    if (finalX < minValidX) return { error: 'noGap', cands };

    const bgRect = bgImg.getBoundingClientRect();
    const scale = bgRect.width / bg.w;
    const targetLeft = (finalX - pzMinX) * scale;
    const hb = slider.getBoundingClientRect(), tb = track.getBoundingClientRect();
    const pzRect = pzImg.getBoundingClientRect();
    return {
        targetLeft, score: finalScore, method,
        handleX: hb.x + hb.width / 2, handleY: hb.y + hb.height / 2,
        handleW: hb.width, trackW: tb.width,
        bgDispW: bgRect.width, pieceDispW: pzRect.width,
    };
}"""


async def _detect_target_in_page(page: Any) -> dict[str, Any] | None:
    try:
        return await page.evaluate(_DETECT_JS)
    except Exception:
        return None


async def _piece_left(page: Any) -> float:
    try:
        return await page.evaluate(
            "() => parseFloat((document.getElementById('aliyunCaptcha-puzzle')||{style:{}}).style.left) || 0"
        )
    except Exception:
        return 0.0


async def _feedback_drag(page: Any, det: dict[str, Any]) -> float:
    # Aliyun (F015) scores drag KINEMATICS, not just final position. A monotonic
    # sweep straight to target lands pixel-correct yet is flagged non-human. Humans
    # accelerate, ballistically overshoot, then correct back. So: (1) ballistic
    # ease-out to an overshoot past target with y-wobble, (2) closed-loop correction
    # reading the piece live style.left back to target +/-2px, (3) settle + release.
    target = float(det["targetLeft"])
    sx = float(det["handleX"])
    sy = float(det["handleY"])
    handle_w = float(det.get("handleW", 40) or 40)
    track_w = float(det.get("trackW", 300) or 300)
    piece_dw = float(det.get("pieceDispW", 52) or 52)
    bg_dw = float(det.get("bgDispW", 300) or 300)

    handle_range = max(1.0, track_w - handle_w)
    piece_range = max(1.0, bg_dw - piece_dw)
    ratio = handle_range / piece_range

    await page.mouse.move(sx, sy, steps=2)
    await asyncio.sleep(random.uniform(0.06, 0.14))
    await page.mouse.down()
    await asyncio.sleep(random.uniform(0.05, 0.10))

    overshoot = random.uniform(14.0, 26.0)
    peak_x = sx + (target + overshoot) * ratio
    span = peak_x - sx
    n = random.randint(34, 46)
    for i in range(1, n + 1):
        t = i / n
        ease = 1 - (1 - t) ** 3
        x = sx + span * ease
        yj = sy + math.sin(t * math.pi) * random.uniform(1.5, 3.5)
        await page.mouse.move(x, yj, steps=1)
        await asyncio.sleep(0.010 + (i % 5) * 0.004)
        if i == n // 2 and random.random() < 0.5:
            await asyncio.sleep(random.uniform(0.04, 0.10))

    cur_x = peak_x
    left = await _piece_left(page)
    for _ in range(40):
        left = await _piece_left(page)
        remaining = target - left
        if abs(remaining) <= 2.0:
            break
        delta = max(-40.0, min(40.0, remaining * ratio)) * random.uniform(0.5, 0.85)
        cur_x += delta + random.uniform(-0.8, 0.8)
        await page.mouse.move(cur_x, sy + random.uniform(-0.8, 0.8), steps=3)
        await asyncio.sleep(random.uniform(0.02, 0.045))

    await asyncio.sleep(random.uniform(0.12, 0.28))
    await page.mouse.up()
    return left


_VERDICT_JS = r"""() => {
    const w = document.getElementById('aliyunCaptcha-captcha-wrapper');
    const wrapperVisible = !!(w && w.offsetParent !== null);
    const slider = document.getElementById('aliyunCaptcha-sliding-slider');
    const sliderW = slider ? slider.getBoundingClientRect().width : 0;
    const body = (document.body ? document.body.innerText : '').toLowerCase();
    // server-side rejection (F015). qoder shows "verify captcha failed" + Request ID
    // when its backend VerifyCaptchaV3 call is flagged — note "captcha" sits between
    // "verify" and "failed", so a plain "verify failed" match misses it.
    const fail = body.includes('captcha failed') || body.includes('verify failed')
        || body.includes('verification failed') || body.includes('unable to verify');
    const advanced = body.includes('verify your email') || body.includes('enter the code');
    return { wrapperVisible, sliderW, fail, advanced };
}"""


async def _await_verdict(page: Any, timeout_s: float = 12.0) -> str:
    # Authoritative success = the FLOW ADVANCED (page now shows "verify your email" /
    # "enter the code"). That is the only unambiguous "solved" signal and is checked
    # FIRST, so a lingering/stale "verify captcha failed" banner can never override a
    # real page change. Only if the flow has NOT advanced do we treat the fail banner
    # (server F015 rejection) as fail. Give an initial settle so we do not conclude
    # before the ~1-3s server round-trip lands.
    await asyncio.sleep(2.0)
    deadline = time.monotonic() + timeout_s
    saw_fail = False
    while time.monotonic() < deadline:
        try:
            v = await page.evaluate(_VERDICT_JS)
        except Exception:
            v = None
        if v:
            if v.get("advanced"):
                return "ok"
            if v.get("fail"):
                saw_fail = True
            widget_gone = not v.get("wrapperVisible") or v.get("sliderW", 0) == 0
            if widget_gone and not saw_fail:
                return "ok"
        await asyncio.sleep(0.5)
    return "fail" if saw_fail else "timeout"


async def _captcha_present(page: Any) -> bool:
    try:
        return await page.evaluate(
            "() => { const s = document.getElementById('aliyunCaptcha-sliding-slider')"
            " || document.querySelector('.slider-move');"
            " return !!s && s.getBoundingClientRect().width > 0; }"
        )
    except Exception:
        return False


async def _detect_fallback(page: Any) -> dict[str, Any] | None:
    geo = await _read_captcha_geometry(page)
    if not geo:
        return None
    target = detect_gap_offset(
        geo["back"], geo["piece"], geo["puzzleDispW"], geo["trackW"], geo["handleW"]
    )
    if target is None or target <= 0:
        return None
    return {
        "targetLeft": target,
        "score": 0.0,
        "method": "numpy-fallback",
        "handleX": geo["handleX"],
        "handleY": geo["handleY"],
        "handleW": geo["handleW"],
        "trackW": geo["trackW"],
        "bgDispW": geo["puzzleDispW"],
        "pieceDispW": 52.0,
    }


async def _open_gate(page: Any) -> None:
    try:
        await page.evaluate(
            "() => { const l = document.getElementById('aliyunCaptcha-captcha-left');"
            " if (l && l.offsetParent !== null) l.click(); }"
        )
    except Exception:
        pass


async def _flow_advanced(page: Any) -> bool:
    try:
        v = await page.evaluate(_VERDICT_JS)
        return bool(v and v.get("advanced"))
    except Exception:
        return False


async def _wait_captcha(page: Any, timeout_s: float = 25.0) -> bool:
    # With a proxy the widget can load slowly. Poll until the slider appears (opening
    # the "click to verify" gate if present). Return False only if the flow already
    # advanced (real success) or nothing showed up before the deadline.
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if await _captcha_present(page):
            return True
        if await _flow_advanced(page):
            return False
        await _open_gate(page)
        await asyncio.sleep(0.6)
    return False


async def solve_slide_captcha(
    page: Any,
    prog: Progress,
    email: str,
    max_attempts: int = 8,
) -> bool:
    if not _HAVE_CV:
        prog.log("numpy/Pillow missing — cannot solve slide captcha", "ERR", email=email, step="captcha")
        return False

    for attempt in range(1, max_attempts + 1):
        if not await _captcha_present(page):
            if await _flow_advanced(page):
                return True
            if not await _wait_captcha(page):
                if await _flow_advanced(page):
                    return True
                prog.log("captcha did not render — retry", "WAIT", email=email, step="captcha")
                continue
        det = await _detect_target_in_page(page)
        if not det or det.get("error") or det.get("targetLeft", 0) <= 0:
            det = await _detect_fallback(page)
        if not det or det.get("targetLeft", 0) <= 0:
            prog.log("gap detect failed — refresh", "WAIT", email=email, step="captcha")
            await _refresh(page)
            continue
        achieved = await _feedback_drag(page, det)
        prog.log(
            f"slide {attempt}/{max_attempts} target={det['targetLeft']:.0f} "
            f"got={achieved:.0f} [{det.get('method', '?')}]",
            "STEP",
            email=email,
            step="captcha",
        )
        verdict = await _await_verdict(page, timeout_s=12.0)
        if verdict == "ok":
            prog.log("captcha solved", "OK", email=email, step="captcha")
            return True
        prog.log(f"slide {verdict} — retry", "WAIT", email=email, step="captcha")
        await _refresh(page)
    prog.log("slide captcha not solved", "ERR", email=email, step="captcha")
    return False


async def solve_manual(page: Any, prog: Progress, email: str, timeout_s: int = 180) -> bool:
    # Human-in-the-loop fallback: bring the window forward and poll until the person
    # solves the slider (flow advances / widget gone with no fail banner). Used when
    # auto is F015-flagged and a real user finishes it in the visible browser.
    try:
        await page.bring_to_front()
    except Exception:
        pass
    prog.log(
        f"MANUAL captcha: solve the slider in the browser window ({timeout_s}s)",
        "WARN",
        email=email,
        step="captcha",
    )
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if await _flow_advanced(page):
            prog.log("manual captcha solved", "OK", email=email, step="captcha")
            return True
        if not await _captcha_present(page):
            v = await _await_verdict(page, timeout_s=2.0)
            if v == "ok":
                prog.log("manual captcha solved", "OK", email=email, step="captcha")
                return True
        await asyncio.sleep(1.5)
    prog.log("manual captcha timed out", "ERR", email=email, step="captcha")
    return False


async def handle_captcha(
    page: Any,
    cfg: Any,
    prog: Progress,
    email: str,
) -> bool:
    mode = (getattr(cfg, "captcha_mode", "auto") or "auto").lower()
    manual_timeout = int(getattr(cfg, "captcha_manual_timeout", 180) or 180)
    if mode == "manual":
        return await solve_manual(page, prog, email, manual_timeout)
    solved = await solve_slide_captcha(page, prog, email)
    if solved:
        return True
    if mode == "auto-then-manual":
        prog.log("auto failed — handing off to manual", "WARN", email=email, step="captcha")
        return await solve_manual(page, prog, email, manual_timeout)
    return False


async def _refresh(page: Any) -> None:
    try:
        await page.evaluate(
            "() => { const l = document.getElementById('aliyunCaptcha-captcha-left');"
            " if (l && l.offsetParent !== null) l.click(); }"
        )
        await asyncio.sleep(1.0)
        btn = page.get_by_role("button", name=REFRESH_LABEL)
        if await btn.count() > 0:
            await btn.first.click()
            await asyncio.sleep(1.5)
    except Exception:
        pass
