#!/usr/bin/env bash
# Assemble dlss5oneclick-x86_64.AppImage from a release build.
# Usage: packaging/build-appimage.sh [path-to-binary] [output-dir]
# Runs on CI (no FUSE: appimagetool via --appimage-extract-and-run) and locally.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(dirname "$here")"
bin="${1:-$repo/target/release/dlss5oneclick}"
out="${2:-$repo/target}"

[ -f "$bin" ] || { echo "binary not found: $bin (cargo build --release first)"; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
appdir="$work/AppDir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" "$appdir/usr/share/icons/hicolor/256x256/apps"

install -m 755 "$bin" "$appdir/usr/bin/dlss5oneclick"
install -m 644 "$here/dlss5oneclick.desktop" "$appdir/dlss5oneclick.desktop"
install -m 644 "$here/dlss5oneclick.desktop" "$appdir/usr/share/applications/dlss5oneclick.desktop"
install -m 644 "$repo/assets/icon-256.png" "$appdir/dlss5oneclick.png"
install -m 644 "$repo/assets/icon-256.png" "$appdir/usr/share/icons/hicolor/256x256/apps/dlss5oneclick.png"
ln -sf usr/bin/dlss5oneclick "$appdir/AppRun"

# Pinned appimagetool release ("continuous" is the project's rolling tag; the
# binary is stable). Cached next to the output for local reruns.
tool="$out/appimagetool-x86_64.AppImage"
if [ ! -x "$tool" ]; then
    curl -fsSL -o "$tool" \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
    chmod +x "$tool"
fi

ARCH=x86_64 "$tool" --appimage-extract-and-run "$appdir" "$out/dlss5oneclick-x86_64.AppImage"
echo "built $out/dlss5oneclick-x86_64.AppImage"
