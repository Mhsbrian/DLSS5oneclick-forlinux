#!/usr/bin/env bash
# Prepare a bare ubuntu:22.04 container to build a release: the tools the
# build needs (cmake for aws-lc-sys, zsync so appimagetool writes the .zsync,
# desktop-file-utils so it validates the desktop entry) and a stable Rust.
# release.yml runs it on CI; release-build.sh shows the local equivalent.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    ca-certificates curl git build-essential cmake pkg-config file zsync desktop-file-utils
curl -fsSL --proto '=https' --tlsv1.2 https://sh.rustup.rs |
    sh -s -- -y -q --profile minimal --default-toolchain stable --component clippy
# On Actions, later steps find cargo through GITHUB_PATH.
if [ -n "${GITHUB_PATH:-}" ]; then
    echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
fi
