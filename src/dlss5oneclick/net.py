"""HTTP helpers: GitHub API JSON, streamed downloads, safe zip extraction."""
from __future__ import annotations

import json
import re
import ssl
import urllib.error
import urllib.request
import zipfile
from pathlib import Path
from typing import Callable

from dlss5oneclick import __version__

ProgressCB = Callable[[int, str], None]
USER_AGENT = f"DLSS5oneclick/{__version__}"


def _noop(pct: int, msg: str) -> None:
    pass


class NetError(Exception):
    """Transport / parse failure with a message safe to show in the UI."""


def _ssl_context() -> ssl.SSLContext:
    try:
        import certifi  # type: ignore

        return ssl.create_default_context(cafile=certifi.where())
    except ImportError:
        return ssl.create_default_context()


def get_json(url: str, timeout: int = 20) -> object:
    req = urllib.request.Request(
        url, headers={"User-Agent": USER_AGENT, "Accept": "application/vnd.github+json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_context()) as resp:
            return json.loads(resp.read())
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
        raise NetError(f"Request failed for {url}: {e}") from e


def get_text(url: str, timeout: int = 20) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_context()) as resp:
            return resp.read().decode("utf-8", errors="replace")
    except (urllib.error.URLError, TimeoutError) as e:
        raise NetError(f"Request failed for {url}: {e}") from e


def download(url: str, dest: Path, label: str, progress: ProgressCB = _noop,
             timeout: int = 60) -> Path:
    """Stream `url` into `dest`, reporting percent + KB/MB downloaded."""
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".part")
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_context()) as resp:
            total = int(resp.headers.get("Content-Length") or 0)
            done = 0
            with open(tmp, "wb") as f:
                while True:
                    chunk = resp.read(1 << 18)
                    if not chunk:
                        break
                    f.write(chunk)
                    done += len(chunk)
                    if total:
                        progress(min(99, done * 100 // total),
                                 f"{label}: {_fmt(done)} / {_fmt(total)}")
                    else:
                        progress(0, f"{label}: {_fmt(done)}")
    except (urllib.error.URLError, TimeoutError) as e:
        tmp.unlink(missing_ok=True)
        raise NetError(f"Download failed for {label}: {e}") from e
    tmp.replace(dest)
    return dest


def _fmt(n: int) -> str:
    if n >= 1 << 20:
        return f"{n / (1 << 20):.1f} MB"
    return f"{n // 1024} KB"


def safe_extract_member(zf: zipfile.ZipFile, member: str, dest: Path) -> Path:
    """Extract one member to an exact destination path (no path from the zip is trusted)."""
    dest.parent.mkdir(parents=True, exist_ok=True)
    with zf.open(member) as src, open(dest, "wb") as out:
        while True:
            chunk = src.read(1 << 18)
            if not chunk:
                break
            out.write(chunk)
    return dest


def zip_members(zf: zipfile.ZipFile, pattern: str) -> list[str]:
    """Members whose name matches regex `pattern` (case-insensitive), files only."""
    rx = re.compile(pattern, re.IGNORECASE)
    return [i.filename for i in zf.infolist()
            if not i.is_dir() and rx.search(i.filename)]
