"""Game-folder inspection: exe bitness, ReShade presence, installed pieces."""
from __future__ import annotations

import struct
from dataclasses import dataclass, field
from pathlib import Path

PE_X64 = 0x8664
PE_X86 = 0x014C

FEEDER_ADDON = "dlss5-feed.addon64"
FEEDER_FX = "DLSS5_Feed.fx"
DLSS5_ADDON = "renodx-dlss5.addon64"
DLSSNR_DLL = "nvngx_dlssnr.dll"
DLSS_DLL = "nvngx_dlss.dll"
LUMENITE_KERNEL_FX = "lumenite_Kernel.fx"
LUMENITE_BLUENOISE = "lumenite_bluenoise256.png"
RESHADE_PROXY = "dxgi.dll"
RESHADE_INI = "ReShade.ini"
RESHADE_PRESET = "ReShadePreset.ini"


class GameError(Exception):
    pass


def exe_bitness(exe: Path) -> int:
    """Return 64 or 32 from the PE header. Raises GameError on a non-PE file."""
    try:
        with open(exe, "rb") as f:
            head = f.read(0x40)
            if len(head) < 0x40 or head[:2] != b"MZ":
                raise GameError(f"{exe.name} is not a Windows executable")
            (pe_off,) = struct.unpack_from("<I", head, 0x3C)
            f.seek(pe_off)
            sig = f.read(4)
            if sig != b"PE\0\0":
                raise GameError(f"{exe.name} has no PE header")
            (machine,) = struct.unpack("<H", f.read(2))
    except OSError as e:
        raise GameError(f"Cannot read {exe.name}: {e}") from e
    if machine == PE_X64:
        return 64
    if machine == PE_X86:
        return 32
    raise GameError(f"{exe.name}: unsupported machine type 0x{machine:04x}")


def is_reshade_dll(path: Path) -> bool:
    """True if `path` is a ReShade proxy DLL (the binary carries a 'ReShade' string)."""
    try:
        if not path.is_file() or path.stat().st_size < 1 << 20:
            return False
        with open(path, "rb") as f:
            return b"ReShade" in f.read()
    except OSError:
        return False


@dataclass(frozen=True)
class GameStatus:
    exe: Path
    bitness: int
    reshade: bool
    feeder: bool
    lumenite: bool
    dlss5_addon: bool
    dlssnr: bool
    dlss: bool
    problems: list[str] = field(default_factory=list)

    @property
    def game_dir(self) -> Path:
        return self.exe.parent

    @property
    def complete(self) -> bool:
        return all((self.reshade, self.feeder, self.lumenite,
                    self.dlss5_addon, self.dlssnr, self.dlss))


def inspect(exe: Path) -> GameStatus:
    exe = Path(exe)
    if not exe.is_file():
        raise GameError(f"Game executable not found: {exe}")
    d = exe.parent
    bits = exe_bitness(exe)
    shaders = d / "reshade-shaders" / "Shaders"
    textures = d / "reshade-shaders" / "Textures"
    problems: list[str] = []
    if bits != 64:
        problems.append("32-bit game: DLSS5-Feeder needs the host64 setup, "
                        "which this tool does not automate yet.")
    if (d / "d3d9.dll").is_file() and not (d / RESHADE_PROXY).is_file():
        problems.append("A d3d9.dll proxy is present; DirectX 9 games are not supported here.")
    return GameStatus(
        exe=exe,
        bitness=bits,
        reshade=is_reshade_dll(d / RESHADE_PROXY),
        feeder=(d / FEEDER_ADDON).is_file() and (shaders / FEEDER_FX).is_file(),
        lumenite=(shaders / LUMENITE_KERNEL_FX).is_file()
        and (textures / LUMENITE_BLUENOISE).is_file(),
        dlss5_addon=(d / DLSS5_ADDON).is_file(),
        dlssnr=(d / DLSSNR_DLL).is_file(),
        dlss=(d / DLSS_DLL).is_file(),
        problems=problems,
    )
