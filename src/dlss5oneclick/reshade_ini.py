"""Minimal ReShade .ini reader/writer.

ReShade's own ini_file stores multi-values comma-separated and escapes a
literal comma as ",,". Key names verified against crosire/reshade
source/runtime.cpp: [GENERAL] EffectSearchPaths / TextureSearchPaths /
PreprocessorDefinitions / PresetPath in ReShade.ini; Techniques and
PreprocessorDefinitions in the preset's root (section-less) block.
Techniques entries are "Name@File.fx".
"""
from __future__ import annotations

from pathlib import Path

MV_PROVIDER_DEFINE = "DLSS5_MV_PROVIDER=3"  # LumeniteFX Kernel
TECHNIQUES_ORDERED = [
    "Lumenite_Kernel@lumenite_Kernel.fx",  # provider must sit ABOVE the feed
    "DLSS5_Feed@DLSS5_Feed.fx",
]


def parse(text: str) -> dict[str, dict[str, str]]:
    """Section -> key -> raw value. Root (section-less) keys live under ''."""
    out: dict[str, dict[str, str]] = {"": {}}
    section = ""
    for line in text.splitlines():
        s = line.strip()
        if not s or s.startswith((";", "#")):
            continue
        if s.startswith("[") and s.endswith("]"):
            section = s[1:-1]
            out.setdefault(section, {})
            continue
        if "=" in s:
            k, v = s.split("=", 1)
            out.setdefault(section, {})[k.strip()] = v.strip()
    return out


def dump(data: dict[str, dict[str, str]]) -> str:
    lines: list[str] = []
    for k, v in data.get("", {}).items():
        lines.append(f"{k}={v}")
    for section, kv in data.items():
        if section == "":
            continue
        if lines:
            lines.append("")
        lines.append(f"[{section}]")
        for k, v in kv.items():
            lines.append(f"{k}={v}")
    return "\n".join(lines) + "\n"


def split_list(raw: str) -> list[str]:
    """Split a ReShade multi-value on single commas; ',,' is an escaped comma."""
    items: list[str] = []
    cur = ""
    i = 0
    while i < len(raw):
        c = raw[i]
        if c == ",":
            if i + 1 < len(raw) and raw[i + 1] == ",":
                cur += ","
                i += 2
                continue
            items.append(cur)
            cur = ""
        else:
            cur += c
        i += 1
    if cur or items:
        items.append(cur)
    return [x for x in items if x != ""]


def join_list(items: list[str]) -> str:
    return ",".join(x.replace(",", ",,") for x in items)


def _ensure_define(raw: str, define: str) -> str:
    name = define.split("=", 1)[0]
    items = [d for d in split_list(raw) if d.split("=", 1)[0] != name]
    items.append(define)
    return join_list(items)


def load(path: Path) -> dict[str, dict[str, str]]:
    if path.is_file():
        return parse(path.read_text(encoding="utf-8", errors="replace"))
    return {"": {}}


def write_reshade_ini(game_dir: Path) -> Path:
    """Create or update ReShade.ini so shaders/textures resolve and the
    DLSS5 provider define is set globally."""
    p = game_dir / "ReShade.ini"
    data = load(p)
    g = data.setdefault("GENERAL", {})
    g.setdefault("EffectSearchPaths", r".\reshade-shaders\Shaders\**")
    g.setdefault("TextureSearchPaths", r".\reshade-shaders\Textures\**")
    g.setdefault("PresetPath", r".\ReShadePreset.ini")
    g["PreprocessorDefinitions"] = _ensure_define(
        g.get("PreprocessorDefinitions", ""), MV_PROVIDER_DEFINE)
    p.write_text(dump(data), encoding="utf-8")
    return p


def write_preset(game_dir: Path) -> Path:
    """Create or update ReShadePreset.ini: enable Lumenite Kernel then
    DLSS5 Feed (in that order, ahead of whatever else is enabled) and set
    the provider define at preset level too."""
    p = game_dir / "ReShadePreset.ini"
    data = load(p)
    root = data.setdefault("", {})
    existing = [t for t in split_list(root.get("Techniques", ""))
                if t not in TECHNIQUES_ORDERED]
    root["Techniques"] = join_list(TECHNIQUES_ORDERED + existing)
    if "TechniqueSorting" in root:
        sorted_existing = [t for t in split_list(root["TechniqueSorting"])
                           if t not in TECHNIQUES_ORDERED]
        root["TechniqueSorting"] = join_list(TECHNIQUES_ORDERED + sorted_existing)
    root["PreprocessorDefinitions"] = _ensure_define(
        root.get("PreprocessorDefinitions", ""), MV_PROVIDER_DEFINE)
    p.write_text(dump(data), encoding="utf-8")
    return p
