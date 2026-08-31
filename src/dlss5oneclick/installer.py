"""The five install steps, in the order the DLSS5-Feeder README lists them.

Sources (all verified 2026-08-31):
  1. ReShade with add-on support — https://reshade.me links
     /downloads/ReShade_Setup_<ver>_Addon.exe; that exe carries an appended
     ZIP containing ReShade64.dll / ReShade32.dll. We drop ReShade64.dll
     next to the game exe as dxgi.dll (Direct3D 10/11/12 proxy).
  2. DLSS5-Feeder — github.com/jlrouzies-fr/DLSS5-Feeder latest release ships
     loose assets: dlss5-feed.addon64 and DLSS5_Feed.fx (plus addon32,
     host64 exe and a Vulkan-layer zip we do not use).
  3. LumeniteFX — github.com/umar-afzaal/LumeniteFX (branch `mainline`, no
     releases). Needs Shaders/lumenite_*.fx + Shaders/include/*.fxh into
     reshade-shaders/Shaders and Textures/lumenite_bluenoise256.png into
     reshade-shaders/Textures.
  4. DLSS 5 add-on — github.com/RankFTW/rhi-repo releases: tag prefix
     `renodx-dlss5-` (zip with renodx-dlss5.addon64), `dlssnr-` (zip with
     nvngx_dlssnr.dll), `dlss-` (zip with nvngx_dlss.dll; not dlssg-/dlssd-).
  5. Config — ReShade.ini + ReShadePreset.ini so DLSS5_MV_PROVIDER=3 is set
     and Lumenite_Kernel is enabled above DLSS5_Feed.
"""
from __future__ import annotations

import logging
import re
import shutil
import tempfile
import zipfile
from pathlib import Path

from dlss5oneclick import game, net, reshade_ini
from dlss5oneclick.net import ProgressCB, _noop

logger = logging.getLogger(__name__)

RESHADE_HOME = "https://reshade.me"
RESHADE_ADDON_RX = re.compile(r"/downloads/ReShade_Setup_([\d.]+)_Addon\.exe")
FEEDER_LATEST = "https://api.github.com/repos/jlrouzies-fr/DLSS5-Feeder/releases/latest"
LUMENITE_ZIP = "https://codeload.github.com/umar-afzaal/LumeniteFX/zip/refs/heads/mainline"
RHI_RELEASES = "https://api.github.com/repos/RankFTW/rhi-repo/releases?per_page=100"


class InstallError(Exception):
    """User-facing failure; message is safe to show verbatim."""


def _ver_key(tag: str, prefix: str) -> tuple[int, ...]:
    body = tag[len(prefix):]
    nums = re.findall(r"\d+", body)
    return tuple(int(n) for n in nums)


def pick_latest_asset(releases: list[dict], prefix: str) -> tuple[str, str]:
    """From rhi-repo releases pick the newest tag with exactly `prefix` and
    return (tag, browser_download_url of its first asset)."""
    cands = []
    for r in releases:
        tag = str(r.get("tag_name", ""))
        if not tag.startswith(prefix):
            continue
        rest = tag[len(prefix):]
        if not rest or not rest[0].isdigit():
            continue  # e.g. "dlss-" must not match "dlssg-"
        assets = r.get("assets") or []
        if not assets:
            continue
        cands.append((_ver_key(tag, prefix), tag, assets[0]["browser_download_url"]))
    if not cands:
        raise InstallError(f"No release with tag prefix '{prefix}' found.")
    cands.sort()
    _, tag, url = cands[-1]
    return tag, url


def pick_feeder_assets(release: dict) -> dict[str, str]:
    """Map required loose asset names -> download URL from a Feeder release."""
    want = {game.FEEDER_ADDON.lower(): game.FEEDER_ADDON,
            game.FEEDER_FX.lower(): game.FEEDER_FX}
    out: dict[str, str] = {}
    for a in release.get("assets") or []:
        name = str(a.get("name", ""))
        if name.lower() in want:
            out[want[name.lower()]] = a["browser_download_url"]
    missing = [n for n in want.values() if n not in out]
    if missing:
        raise InstallError(
            f"DLSS5-Feeder release {release.get('tag_name')} is missing: {', '.join(missing)}")
    return out


# ── Step 1: ReShade ───────────────────────────────────────────────────

def resolve_reshade_setup() -> tuple[str, str]:
    html = net.get_text(RESHADE_HOME)
    m = RESHADE_ADDON_RX.search(html)
    if not m:
        raise InstallError("Could not find the ReShade add-on installer link on reshade.me.")
    return m.group(1), RESHADE_HOME + m.group(0)


def install_reshade_from_setup(setup_exe: Path, game_dir: Path, bitness: int) -> list[str]:
    dll = "ReShade64.dll" if bitness == 64 else "ReShade32.dll"
    try:
        with zipfile.ZipFile(setup_exe) as zf:
            if dll not in zf.namelist():
                raise InstallError(f"{setup_exe.name} does not contain {dll}.")
            net.safe_extract_member(zf, dll, game_dir / game.RESHADE_PROXY)
    except zipfile.BadZipFile as e:
        raise InstallError(f"ReShade installer has no readable archive: {e}") from e
    return [game.RESHADE_PROXY]


def step_reshade(status: game.GameStatus, work: Path, progress: ProgressCB) -> list[str]:
    if status.reshade:
        progress(100, "ReShade already installed")
        return []
    proxy = status.game_dir / game.RESHADE_PROXY
    if proxy.is_file():
        raise InstallError(
            f"{proxy.name} exists but is not ReShade (another injector like DXVK or "
            "Special K?). Remove it first.")
    progress(0, "Looking up latest ReShade")
    ver, url = resolve_reshade_setup()
    setup = net.download(url, work / f"ReShade_Setup_{ver}_Addon.exe", "ReShade", progress)
    return install_reshade_from_setup(setup, status.game_dir, status.bitness)


# ── Step 2: DLSS5-Feeder ──────────────────────────────────────────────

def step_feeder(status: game.GameStatus, work: Path, progress: ProgressCB) -> list[str]:
    progress(0, "Looking up latest DLSS5-Feeder")
    release = net.get_json(FEEDER_LATEST)
    if not isinstance(release, dict):
        raise InstallError("Unexpected response from GitHub for DLSS5-Feeder.")
    assets = pick_feeder_assets(release)
    d = status.game_dir
    shaders = d / "reshade-shaders" / "Shaders"
    shaders.mkdir(parents=True, exist_ok=True)
    installed = []
    net.download(assets[game.FEEDER_ADDON], d / game.FEEDER_ADDON, game.FEEDER_ADDON, progress)
    installed.append(game.FEEDER_ADDON)
    net.download(assets[game.FEEDER_FX], shaders / game.FEEDER_FX, game.FEEDER_FX, progress)
    installed.append(f"reshade-shaders/Shaders/{game.FEEDER_FX}")
    return installed


# ── Step 3: LumeniteFX ────────────────────────────────────────────────

def install_lumenite_from_zip(zip_path: Path, game_dir: Path) -> list[str]:
    shaders = game_dir / "reshade-shaders" / "Shaders"
    textures = game_dir / "reshade-shaders" / "Textures"
    installed: list[str] = []
    try:
        with zipfile.ZipFile(zip_path) as zf:
            fx = net.zip_members(zf, r"/Shaders/lumenite_[^/]+\.fx$")
            fxh = net.zip_members(zf, r"/Shaders/include/[^/]+\.fxh$")
            png = net.zip_members(zf, r"/Textures/lumenite_bluenoise256\.png$")
            if not fx or not png:
                raise InstallError("LumeniteFX archive layout changed; shaders or texture not found.")
            for m in fx:
                net.safe_extract_member(zf, m, shaders / Path(m).name)
                installed.append(f"reshade-shaders/Shaders/{Path(m).name}")
            for m in fxh:
                net.safe_extract_member(zf, m, shaders / "include" / Path(m).name)
                installed.append(f"reshade-shaders/Shaders/include/{Path(m).name}")
            for m in png:
                net.safe_extract_member(zf, m, textures / Path(m).name)
                installed.append(f"reshade-shaders/Textures/{Path(m).name}")
    except zipfile.BadZipFile as e:
        raise InstallError(f"LumeniteFX download is not a valid zip: {e}") from e
    return installed


def step_lumenite(status: game.GameStatus, work: Path, progress: ProgressCB) -> list[str]:
    z = net.download(LUMENITE_ZIP, work / "LumeniteFX.zip", "LumeniteFX", progress)
    return install_lumenite_from_zip(z, status.game_dir)


# ── Step 4: DLSS 5 add-on + models ────────────────────────────────────

def _install_single_from_zip(zip_path: Path, member_name: str, dest: Path) -> None:
    try:
        with zipfile.ZipFile(zip_path) as zf:
            hits = [n for n in zf.namelist() if Path(n).name.lower() == member_name.lower()]
            if not hits:
                raise InstallError(f"{zip_path.name} does not contain {member_name}.")
            net.safe_extract_member(zf, hits[0], dest)
    except zipfile.BadZipFile as e:
        raise InstallError(f"{zip_path.name} is not a valid zip: {e}") from e


def step_dlss5(status: game.GameStatus, work: Path, progress: ProgressCB) -> list[str]:
    progress(0, "Looking up DLSS 5 add-on releases")
    releases = net.get_json(RHI_RELEASES)
    if not isinstance(releases, list):
        raise InstallError("Unexpected response from GitHub for rhi-repo.")
    d = status.game_dir
    installed = []
    plan = [
        ("renodx-dlss5-", game.DLSS5_ADDON, status.dlss5_addon),
        ("dlssnr-", game.DLSSNR_DLL, status.dlssnr),
        ("dlss-", game.DLSS_DLL, status.dlss),
    ]
    for prefix, fname, present in plan:
        if present:
            continue
        tag, url = pick_latest_asset(releases, prefix)
        z = net.download(url, work / f"{tag}.zip", fname, progress)
        _install_single_from_zip(z, fname, d / fname)
        installed.append(f"{fname} ({tag})")
    if not installed:
        progress(100, "DLSS 5 add-on already present")
    return installed


# ── Step 5: config ────────────────────────────────────────────────────

def step_config(status: game.GameStatus, work: Path, progress: ProgressCB) -> list[str]:
    reshade_ini.write_reshade_ini(status.game_dir)
    reshade_ini.write_preset(status.game_dir)
    progress(100, "ReShade.ini + ReShadePreset.ini written")
    return [game.RESHADE_INI, game.RESHADE_PRESET]


STEPS = [
    ("ReShade (add-on build)", step_reshade),
    ("DLSS5-Feeder", step_feeder),
    ("LumeniteFX motion vectors", step_lumenite),
    ("DLSS 5 add-on + models", step_dlss5),
    ("ReShade config", step_config),
]


def run_all(exe: Path, progress: ProgressCB = _noop,
            step_cb=None) -> dict[str, list[str]]:
    """Run every step. `step_cb(index, name, state, detail)` with state in
    {"start", "done", "error"}. Raises InstallError on the first failure."""
    status = game.inspect(exe)
    if status.problems:
        raise InstallError("\n".join(status.problems))
    results: dict[str, list[str]] = {}
    with tempfile.TemporaryDirectory(prefix="dlss5oneclick-") as tmp:
        work = Path(tmp)
        for i, (name, fn) in enumerate(STEPS):
            if step_cb:
                step_cb(i, name, "start", "")
            try:
                files = fn(status, work, progress)
            except (InstallError, net.NetError, game.GameError, OSError) as e:
                if step_cb:
                    step_cb(i, name, "error", str(e))
                raise InstallError(f"{name}: {e}") from e
            results[name] = files
            if step_cb:
                step_cb(i, name, "done", ", ".join(files) if files else "already present")
            status = game.inspect(exe)
    return results


def uninstall(exe: Path) -> list[str]:
    """Remove everything this tool places. Leaves ReShade + its ini alone if
    the user had shaders of their own (only removes our files)."""
    d = Path(exe).parent
    removed = []
    targets = [
        d / game.FEEDER_ADDON, d / game.DLSS5_ADDON, d / game.DLSSNR_DLL,
        d / "reshade-shaders" / "Shaders" / game.FEEDER_FX,
        d / "reshade-shaders" / "Textures" / game.LUMENITE_BLUENOISE,
    ]
    targets += list((d / "reshade-shaders" / "Shaders").glob("lumenite_*.fx"))
    targets += list((d / "reshade-shaders" / "Shaders" / "include").glob("lumenite_*.fxh"))
    for t in targets:
        if t.is_file():
            t.unlink()
            removed.append(str(t.relative_to(d)))
    inc = d / "reshade-shaders" / "Shaders" / "include"
    if inc.is_dir() and not any(inc.iterdir()):
        shutil.rmtree(inc)
    return removed
