#!/usr/bin/env bash
# Builds the web UI bundle (crates/teamcraft-ui/dist) with Trunk in the CI's Rust container. The
# Flatpak SDK's Rust extension has no wasm32 target, and the bundle is the same on every
# platform, so the Flatpak build takes it as a prebuilt input. Cargo and Trunk caches go to
# TEAMCRAFT_UI_BUILD_DIR (default ~/.cache/teamcraft-ui-build).
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../../../.." && pwd)"
CACHE="${TEAMCRAFT_UI_BUILD_DIR:-${HOME}/.cache/teamcraft-ui-build}"
IMAGE="${TEAMCRAFT_RUST_IMAGE:-rust:1.99.1-trixie}"
TRUNK_VERSION=0.22.0
mkdir -p "${CACHE}"

# As root (the image's toolchain needs the wasm32 target added); everything written to the mounts
# is handed back to the calling user at the end.
docker run --rm \
  -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target \
  -v "${REPO}:/src" -v "${CACHE}:/cache" -w /src/crates/teamcraft-ui "${IMAGE}" bash -c "
    set -euo pipefail
    trap 'chown -R $(id -u):$(id -g) /cache /src/crates/teamcraft-ui/dist 2>/dev/null || true' EXIT
    export PATH=/cache/cargo/bin:\$PATH
    rustup target add wasm32-unknown-unknown
    command -v trunk >/dev/null || cargo install --locked trunk@${TRUNK_VERSION}
    # Same size settings as tauri.conf.json's beforeBuildCommand.
    CARGO_PROFILE_RELEASE_OPT_LEVEL=s CARGO_PROFILE_RELEASE_LTO=true trunk build --release
  "
echo "UI bundle: ${REPO}/crates/teamcraft-ui/dist"
