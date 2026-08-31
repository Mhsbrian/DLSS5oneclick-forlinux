import struct
from pathlib import Path

import pytest

from dlss5oneclick import game


def make_pe(path: Path, machine: int) -> Path:
    head = bytearray(0x40)
    head[:2] = b"MZ"
    struct.pack_into("<I", head, 0x3C, 0x40)
    pe = b"PE\0\0" + struct.pack("<H", machine) + b"\0" * 18
    path.write_bytes(bytes(head) + pe)
    return path


def make_reshade_dll(path: Path) -> Path:
    path.write_bytes(b"MZ" + b"\0" * (1 << 20) + b"ReShade" + b"\0" * 16)
    return path


def test_bitness_x64_and_x86(tmp_path):
    assert game.exe_bitness(make_pe(tmp_path / "a.exe", game.PE_X64)) == 64
    assert game.exe_bitness(make_pe(tmp_path / "b.exe", game.PE_X86)) == 32


def test_bitness_rejects_non_pe(tmp_path):
    p = tmp_path / "x.exe"
    p.write_bytes(b"hello")
    with pytest.raises(game.GameError):
        game.exe_bitness(p)


def test_is_reshade_dll_requires_marker_and_size(tmp_path):
    small = tmp_path / "dxgi.dll"
    small.write_bytes(b"ReShade")
    assert not game.is_reshade_dll(small)
    assert game.is_reshade_dll(make_reshade_dll(tmp_path / "real.dll"))


def test_inspect_empty_game(tmp_path):
    exe = make_pe(tmp_path / "game.exe", game.PE_X64)
    st = game.inspect(exe)
    assert st.bitness == 64
    assert not st.reshade and not st.feeder and not st.lumenite
    assert not st.dlss5_addon and not st.dlssnr and not st.dlss
    assert not st.complete
    assert st.problems == []


def test_inspect_flags_32bit(tmp_path):
    exe = make_pe(tmp_path / "game.exe", game.PE_X86)
    st = game.inspect(exe)
    assert st.bitness == 32
    assert any("32-bit" in p for p in st.problems)


def test_inspect_complete(tmp_path):
    exe = make_pe(tmp_path / "game.exe", game.PE_X64)
    make_reshade_dll(tmp_path / "dxgi.dll")
    sh = tmp_path / "reshade-shaders" / "Shaders"
    tx = tmp_path / "reshade-shaders" / "Textures"
    sh.mkdir(parents=True)
    tx.mkdir(parents=True)
    for f in (game.FEEDER_ADDON, game.DLSS5_ADDON, game.DLSSNR_DLL, game.DLSS_DLL):
        (tmp_path / f).write_bytes(b"x")
    (sh / game.FEEDER_FX).write_text("technique DLSS5_Feed {}")
    (sh / game.LUMENITE_KERNEL_FX).write_text("technique Lumenite_Kernel {}")
    (tx / game.LUMENITE_BLUENOISE).write_bytes(b"png")
    assert game.inspect(exe).complete
