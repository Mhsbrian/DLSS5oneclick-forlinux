#!/usr/bin/env bash
# Assemble dlss5oneclick-x86_64.AppImage -- and its .zsync, when zsyncmake is
# installed -- from a release build.
#
#   packaging/build-appimage.sh [path-to-binary] [output-dir]
#
# release-build.sh runs it on CI; it also runs on its own. appimagetool and
# the AppImage runtime -- the loader every user actually executes -- are pinned
# by version and SHA-256, so a release carries exactly the bytes reviewed here
# and an upstream asset replaced under the same name fails the build instead of
# slipping in (appimagetool's 1.9.1 asset has already been re-uploaded once).
# Building needs no FUSE (--appimage-extract-and-run), and the static type2
# runtime needs no libfuse2 on the user's machine.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(dirname "$here")"
bin="${1:-$repo/target/release/dlss5oneclick}"
out="${2:-$repo/target}"

APPIMAGETOOL_VERSION=1.9.1
APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
RUNTIME_VERSION=20251108
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
APP_ID=io.github.mhsbrian.dlss5oneclick

# Where AppImage managers (AppImageUpdate, Gear Lever) look for a newer
# release; the .zsync beside the AppImage turns that into a delta download.
# GITHUB_REPOSITORY is set on Actions, so a fork's release points at the fork.
slug="${GITHUB_REPOSITORY:-Mhsbrian/DLSS5oneclick-forlinux}"
update_info="gh-releases-zsync|${slug%%/*}|${slug##*/}|latest|dlss5oneclick-x86_64.AppImage.zsync"

[ -x "$bin" ] || { echo "binary not found: $bin (cargo build --release first)" >&2; exit 1; }
# The binary names its own version; the AppImage's metadata carries the same.
version="$("$bin" --version | awk '{print $2}')"
[ -n "$version" ] || { echo "$bin --version printed no version" >&2; exit 1; }

cache="${XDG_CACHE_HOME:-$HOME/.cache}/dlss5oneclick-packaging"
mkdir -p "$cache" "$out"
out="$(cd "$out" && pwd)"
# Download a pinned file once, and never use it unless the checksum matches.
fetch() { # url dest sha256
    local url="$1" dest="$2" sum="$3"
    if [ -f "$dest" ] && echo "$sum  $dest" | sha256sum -c --status; then
        return
    fi
    curl -fsSL --retry 3 -o "$dest.part" "$url"
    if ! echo "$sum  $dest.part" | sha256sum -c --status; then
        rm -f "$dest.part"
        echo "checksum mismatch for $url -- refusing to build with it" >&2
        exit 1
    fi
    mv "$dest.part" "$dest"
}
tool="$cache/appimagetool-$APPIMAGETOOL_VERSION-x86_64.AppImage"
runtime="$cache/runtime-$RUNTIME_VERSION-x86_64"
fetch "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-x86_64.AppImage" \
    "$tool" "$APPIMAGETOOL_SHA256"
fetch "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VERSION/runtime-x86_64" \
    "$runtime" "$RUNTIME_SHA256"
chmod +x "$tool"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
appdir="$work/AppDir"
install -Dm755 "$bin" "$appdir/usr/bin/dlss5oneclick"
install -Dm644 "$here/dlss5oneclick.desktop" "$appdir/usr/share/applications/dlss5oneclick.desktop"
install -Dm644 "$here/$APP_ID.metainfo.xml" "$appdir/usr/share/metainfo/$APP_ID.metainfo.xml"
install -Dm644 "$repo/assets/icon-256.png" "$appdir/usr/share/icons/hicolor/256x256/apps/dlss5oneclick.png"
install -Dm644 "$repo/assets/icon-64.png" "$appdir/usr/share/icons/hicolor/64x64/apps/dlss5oneclick.png"
# appimagetool reads the desktop entry and the icon from the AppDir root.
cp "$here/dlss5oneclick.desktop" "$appdir/dlss5oneclick.desktop"
cp "$repo/assets/icon-256.png" "$appdir/dlss5oneclick.png"
ln -s usr/bin/dlss5oneclick "$appdir/AppRun"

target="$out/dlss5oneclick-x86_64.AppImage"
rm -f "$target" "$target.zsync"
# Run from the output folder, where the .zsync has to end up.
(cd "$out" && ARCH=x86_64 VERSION="$version" "$tool" --appimage-extract-and-run \
    --runtime-file "$runtime" \
    --updateinformation "$update_info" \
    "$appdir" "$target")
echo "built $target ($version)"
