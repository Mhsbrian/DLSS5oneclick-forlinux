#!/usr/bin/env bash
# Build, check and package a Linux release into dist/: the portable binary,
# the AppImage with its .zsync, and SHA256SUMS.
#
#   packaging/release-build.sh [vX.Y.Z]
#
# release.yml runs exactly this inside ubuntu:22.04, whose glibc (2.35) is the
# floor the binary is built against. To reproduce a release build locally:
#
#   docker run --rm -v "$PWD":/src -w /src -e CARGO_TARGET_DIR=/tmp/target ubuntu:22.04 \
#     bash -c 'packaging/container-setup.sh && . "$HOME/.cargo/env" && packaging/release-build.sh'
#
# Given a tag, it refuses to build unless the tag, Cargo.toml and both built
# assets carry the same version: the self-updater installs the newest tag, and
# would offer a mislabelled release again on every start.
set -euo pipefail
cd "$(dirname "$0")/.."

tag="${1:-}"
dist=dist
on_ci() { [ -n "${GITHUB_ACTIONS:-}" ]; }
group() { if on_ci; then echo "::group::$*"; else echo "== $*"; fi; }
endgroup() { if on_ci; then echo "::endgroup::"; fi; }
fail() {
    if on_ci; then echo "::error::$*"; else echo "error: $*" >&2; fi
    exit 1
}

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
[ -n "$version" ] || fail "no version in Cargo.toml"
if [ -n "$tag" ] && [ "${tag#v}" != "$version" ]; then
    fail "tag $tag does not match Cargo.toml version $version"
fi

group "clippy"
cargo clippy --all-targets --locked -- -D warnings
endgroup
group "test"
cargo test --locked
endgroup
group "release build"
cargo build --release --locked
endgroup
bin="${CARGO_TARGET_DIR:-target}/release/dlss5oneclick"

group "glibc floor"
floor="$(objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n1)"
echo "$bin needs $floor"
if [ "$(printf '%s\n' "$floor" GLIBC_2.35 | sort -V | tail -n1)" != GLIBC_2.35 ]; then
    fail "the binary needs $floor, above the GLIBC_2.35 release floor -- build inside ubuntu:22.04"
fi
endgroup

group "package"
rm -rf "$dist"
mkdir -p "$dist"
cp "$bin" "$dist/dlss5oneclick-linux-x86_64"
packaging/build-appimage.sh "$bin" "$dist"
endgroup

group "smoke test"
# The self-updater cannot look inside an AppImage -- the binary sits
# compressed in its squashfs -- so the version it would install is checked
# here instead, on both assets.
want="DLSS5oneclick $version"
got_bin="$("$dist/dlss5oneclick-linux-x86_64" --version)"
got_img="$(APPIMAGE_EXTRACT_AND_RUN=1 "$dist/dlss5oneclick-x86_64.AppImage" --version)"
echo "binary:   $got_bin"
echo "AppImage: $got_img"
[ "$got_bin" = "$want" ] || fail "the binary reports '$got_bin', expected '$want'"
[ "$got_img" = "$want" ] || fail "the AppImage reports '$got_img', expected '$want'"
endgroup

(cd "$dist" && sha256sum dlss5oneclick-linux-x86_64 dlss5oneclick-x86_64.AppImage > SHA256SUMS)
cat "$dist/SHA256SUMS"
ls -l "$dist"
