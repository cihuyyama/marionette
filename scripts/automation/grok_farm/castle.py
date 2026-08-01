from __future__ import annotations

import json
import urllib.request
from pathlib import Path
from typing import Any

CASTLE_PK = "pk_p8GGWvD3TmFJZRsX3BQcqAv9aFVispNz"
CASTLE_CDN_URLS = (
    "https://cdn.castle.io/v2/castle.js",
    "https://d2t77mnxyvsf9z.cloudfront.net/v2/castle.js",
)
_CACHE_DIR = Path(__file__).resolve().parent / "data"
_CACHE_PATH = _CACHE_DIR / "castle_v2.js"
_script_source: str = ""


def ensure_cached() -> str:
    global _script_source
    if _script_source:
        return _script_source
    if _CACHE_PATH.is_file() and _CACHE_PATH.stat().st_size > 500:
        _script_source = _CACHE_PATH.read_text(encoding="utf-8", errors="replace")
        return _script_source
    _CACHE_DIR.mkdir(parents=True, exist_ok=True)
    for url in CASTLE_CDN_URLS:
        try:
            req = urllib.request.Request(url, headers={
                "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
                "Accept": "*/*",
            })
            with urllib.request.urlopen(req, timeout=20) as resp:
                body = resp.read()
            text = body.decode("utf-8", "replace")
            if len(text) > 500 and ("_castle" in text or "createRequestToken" in text):
                _CACHE_PATH.write_bytes(body)
                _script_source = text
                return text
        except Exception:
            continue
    return ""


def build_mint_js() -> str:
    src = ensure_cached()
    src_json = json.dumps(src if src else "")
    pk_json = json.dumps(CASTLE_PK)
    cdn_json = json.dumps(CASTLE_CDN_URLS[0])
    return f"""
(async () => {{
  const pk = {pk_json};
  const out = {{ status: 'start', token: '', err: '', api: '' }};
  function accept(tok, api) {{
    const s = String(tok || '');
    if (s.length < 20) return false;
    out.token = s; out.api = String(api || ''); out.status = 'done';
    return true;
  }}
  try {{
    const src = {src_json};
    if (src && src.length > 200) {{
      (0, eval)(src);
      out.status = 'eval-ok';
    }} else if (typeof window._castle !== 'function' && !window.Castle) {{
      await new Promise((resolve, reject) => {{
        const s = document.createElement('script');
        s.src = {cdn_json}; s.async = true;
        s.onload = () => resolve(true);
        s.onerror = () => reject(new Error('castle cdn load failed'));
        (document.head || document.documentElement).appendChild(s);
        setTimeout(() => reject(new Error('castle cdn timeout')), 12000);
      }});
      out.status = 'cdn-ok';
    }}
  }} catch (eLoad) {{ out.err = 'load:' + String(eLoad.message || eLoad); }}
  await new Promise(r => setTimeout(r, 50));
  try {{
    const c = window._castle;
    if (typeof c === 'function') {{
      try {{ c('setAppId', pk); }} catch(e0) {{ try {{ c('setPublishableKey', pk); }} catch(e1) {{}} }}
      let tok = c('createRequestToken');
      if (tok && typeof tok.then === 'function') tok = await tok;
      if (accept(tok, '_castle')) return out;
    }}
  }} catch (eC) {{ out.err = (out.err ? out.err + '|' : '') + '_castle:' + String(eC.message || eC); }}
  try {{
    let api = window.Castle || window.castle || null;
    if (api && api.default) api = api.default;
    if (api && typeof api.configure === 'function') {{ try {{ api.configure({{ pk: pk }}); }} catch(e3) {{}} }}
    if (api && typeof api.createRequestToken === 'function') {{
      let tok = api.createRequestToken();
      if (tok && typeof tok.then === 'function') tok = await tok;
      if (accept(tok, 'Castle')) return out;
    }}
  }} catch (eN) {{ out.err = (out.err ? out.err + '|' : '') + 'Castle:' + String(eN.message || eN); }}
  if (!out.token) out.status = out.status === 'start' ? 'empty' : (out.status + '-empty');
  return out;
}})()
"""


async def mint(page: Any, prog: Any, email: str = "") -> str:
    js = build_mint_js()
    try:
        result = await page.evaluate(js)
        token = (result or {}).get("token", "")
        if token:
            prog.log(f"castle token len={len(token)}", "DBG", email=email, step="castle")
        else:
            status = (result or {}).get("status", "")
            err = (result or {}).get("err", "")
            prog.log(f"castle empty: status={status} err={err}", "WARN", email=email, step="castle")
        return token
    except Exception as e:
        prog.log(f"castle mint error: {e}", "WARN", email=email, step="castle")
        return ""
