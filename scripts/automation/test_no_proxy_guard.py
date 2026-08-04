"""Lock the proxy master switch: no_proxy must empty the pool even when a
proxy file/url/pool env is set. Run: PYTHONPATH=scripts/automation python -m
pytest scripts/automation/test_no_proxy_guard.py  (or plain python execution).
"""

from __future__ import annotations

import os
from dataclasses import replace

from grok_farm import browser as grok_browser
from grok_farm.config import load_config as load_grok
from qoder_farm import browser as qoder_browser
from qoder_farm.config import load_config as load_qoder


def _write_proxies(tmp_path):
    f = tmp_path / "proxies.txt"
    f.write_text("1.2.3.4:8080\n5.6.7.8:9090:user:pass\n", encoding="utf-8")
    return str(f)


def test_qoder_no_proxy_beats_file_and_env(tmp_path, monkeypatch):
    pfile = _write_proxies(tmp_path)
    monkeypatch.setenv("QODER_PROXY_FILE", pfile)
    monkeypatch.setenv("QODER_PROXY_POOL", "9.9.9.9:1111")
    cfg = replace(load_qoder(), proxy_file=pfile, no_proxy=True)
    assert qoder_browser.load_proxy_pool(cfg) == []


def test_qoder_pool_used_when_not_disabled(tmp_path, monkeypatch):
    pfile = _write_proxies(tmp_path)
    monkeypatch.delenv("QODER_PROXY_POOL", raising=False)
    cfg = replace(load_qoder(), proxy_file=pfile, no_proxy=False, proxy_shuffle=False)
    assert len(qoder_browser.load_proxy_pool(cfg)) == 2


def test_grok_no_proxy_beats_file_and_env(tmp_path, monkeypatch):
    pfile = _write_proxies(tmp_path)
    monkeypatch.setenv("GROK_PROXY_FILE", pfile)
    monkeypatch.setenv("GROK_PROXY_POOL", "9.9.9.9:1111")
    cfg = replace(load_grok(), proxy_file=pfile, no_proxy=True)
    assert grok_browser.load_proxy_pool(cfg) == []


if __name__ == "__main__":
    import sys
    import tempfile
    from pathlib import Path

    class _MP:
        def __init__(self):
            self._saved = {}

        def setenv(self, k, v):
            self._saved.setdefault(k, os.environ.get(k))
            os.environ[k] = v

        def delenv(self, k, raising=False):
            self._saved.setdefault(k, os.environ.get(k))
            os.environ.pop(k, None)

        def undo(self):
            for k, v in self._saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v

    failures = 0
    for fn in (
        test_qoder_no_proxy_beats_file_and_env,
        test_qoder_pool_used_when_not_disabled,
        test_grok_no_proxy_beats_file_and_env,
    ):
        mp = _MP()
        with tempfile.TemporaryDirectory() as d:
            try:
                fn(Path(d), mp)
                print(f"PASS {fn.__name__}")
            except AssertionError as e:
                failures += 1
                print(f"FAIL {fn.__name__}: {e}")
            finally:
                mp.undo()
    sys.exit(1 if failures else 0)
