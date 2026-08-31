"""Installer logic against local fakes — no network."""
import zipfile
from pathlib import Path

import pytest

from dlss5oneclick import game, installer
from tests.test_game import make_pe, make_reshade_dll


# ── asset picking against the real release shapes seen 2026-08-31 ─────

FEEDER_RELEASE = {
    "tag_name": "v0.6.0-beta.1",
    "assets": [
        {"name": n, "browser_download_url": f"https://x/{n}"}
        for n in ("dlss5-feed-host64.exe", "dlss5-feed.addon32", "dlss5-feed.addon64",
                  "DLSS5_Feed.fx", "feed-vk-layer.zip")
    ],
}

RHI_RELEASES = [
    {"tag_name": t, "assets": [{"browser_download_url": f"https://x/{t}.zip"}]}
    for t in ("streamline-2.13.0.0", "renodx-dlss5-4.55", "renodx-dlss5-4.5",
              "renodx-dlss5-3.3.4", "dlssnr-310.8.SF-v2", "dlssnr-310.8.SF",
              "dlssg-310.8.0", "dlssd-310.7.129", "dlss-310.8.0", "dlss-310.7.129",
              "DLSS-Enabler-4.9.0.7")
]


def test_pick_feeder_assets_uses_loose_files_not_vk_zip():
    a = installer.pick_feeder_assets(FEEDER_RELEASE)
    assert a == {"dlss5-feed.addon64": "https://x/dlss5-feed.addon64",
                 "DLSS5_Feed.fx": "https://x/DLSS5_Feed.fx"}


def test_pick_feeder_assets_missing_raises():
    rel = {"tag_name": "v9", "assets": [{"name": "feed-vk-layer.zip", "browser_download_url": "u"}]}
    with pytest.raises(installer.InstallError, match="missing"):
        installer.pick_feeder_assets(rel)


def test_pick_latest_asset_versions_and_prefix_isolation():
    assert installer.pick_latest_asset(RHI_RELEASES, "renodx-dlss5-")[0] == "renodx-dlss5-4.55"
    assert installer.pick_latest_asset(RHI_RELEASES, "dlssnr-")[0] == "dlssnr-310.8.SF-v2"
    # "dlss-" must not pick dlssg-/dlssd-/DLSS-Enabler
    assert installer.pick_latest_asset(RHI_RELEASES, "dlss-")[0] == "dlss-310.8.0"


def test_pick_latest_asset_none():
    with pytest.raises(installer.InstallError):
        installer.pick_latest_asset(RHI_RELEASES, "nothing-")


# ── local zip installs ────────────────────────────────────────────────

def _game(tmp_path: Path) -> Path:
    return make_pe(tmp_path / "game.exe", game.PE_X64)


def test_install_reshade_from_setup_extracts_64bit_dll(tmp_path):
    exe = _game(tmp_path)
    setup = tmp_path / "ReShade_Setup_6.8.0_Addon.exe"
    with open(setup, "wb") as f:
        f.write(b"MZ" + b"\0" * 100)  # stub PE prefix
        with zipfile.ZipFile(f, "a") as zf:
            zf.writestr("ReShade64.dll", b"MZ" + b"\0" * (1 << 20) + b"ReShade")
            zf.writestr("ReShade32.dll", b"32")
    installed = installer.install_reshade_from_setup(setup, tmp_path, 64)
    assert installed == ["dxgi.dll"]
    assert game.inspect(exe).reshade


def test_install_lumenite_from_zip_places_shaders_includes_texture(tmp_path):
    exe = _game(tmp_path)
    z = tmp_path / "LumeniteFX.zip"
    with zipfile.ZipFile(z, "w") as zf:
        zf.writestr("LumeniteFX-mainline/README.md", "x")
        zf.writestr("LumeniteFX-mainline/Shaders/lumenite_Kernel.fx", "technique Lumenite_Kernel {}")
        zf.writestr("LumeniteFX-mainline/Shaders/lumenite_TRAA.fx", "t")
        zf.writestr("LumeniteFX-mainline/Shaders/include/lumenite_Helpers.fxh", "h")
        zf.writestr("LumeniteFX-mainline/Textures/lumenite_bluenoise256.png", b"png")
        zf.writestr("../evil.fx", "zip-slip")
    installed = installer.install_lumenite_from_zip(z, tmp_path)
    assert (tmp_path / "reshade-shaders/Shaders/lumenite_Kernel.fx").is_file()
    assert (tmp_path / "reshade-shaders/Shaders/include/lumenite_Helpers.fxh").is_file()
    assert (tmp_path / "reshade-shaders/Textures/lumenite_bluenoise256.png").is_file()
    assert not (tmp_path.parent / "evil.fx").exists()
    assert len(installed) == 4
    assert game.inspect(exe).lumenite


def test_install_lumenite_rejects_wrong_layout(tmp_path):
    z = tmp_path / "bad.zip"
    with zipfile.ZipFile(z, "w") as zf:
        zf.writestr("whatever.txt", "x")
    with pytest.raises(installer.InstallError):
        installer.install_lumenite_from_zip(z, tmp_path)


def test_single_from_zip_and_uninstall(tmp_path):
    exe = _game(tmp_path)
    z = tmp_path / "renodx-dlss5-4.55.zip"
    with zipfile.ZipFile(z, "w") as zf:
        zf.writestr("renodx-dlss5.addon64", b"addon")
    installer._install_single_from_zip(z, "renodx-dlss5.addon64", tmp_path / "renodx-dlss5.addon64")
    assert game.inspect(exe).dlss5_addon
    (tmp_path / game.DLSS_DLL).write_bytes(b"keep")
    removed = installer.uninstall(exe)
    assert "renodx-dlss5.addon64" in removed
    assert (tmp_path / game.DLSS_DLL).is_file()


def test_run_all_refuses_32bit_before_network(tmp_path):
    exe = make_pe(tmp_path / "game.exe", game.PE_X86)
    with pytest.raises(installer.InstallError, match="32-bit"):
        installer.run_all(exe)
