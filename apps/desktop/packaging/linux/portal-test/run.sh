#!/usr/bin/env bash
# Checks screen sharing and the camera through real portals on a headless Wayland session
# (sway + xdg-desktop-portal-wlr + PipeWire) in a container, with the same capture code the
# desktop app uses. Needs Docker (or Podman as `docker`).
#
#   apps/desktop/packaging/linux/portal-test/run.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "${HERE}/../../../../.." && pwd)"
IMAGE=teamcraft-portal-test:local
TARGET="${CARGO_TARGET_DIR:-${REPO}/target}"

docker build -q -t "${IMAGE}" -f "${HERE}/Containerfile" "${HERE}" >/dev/null
# The example is built on the host; the image is Fedora, so build on a Fedora host or in a
# Fedora toolbox with the same release.
cargo build -q -p teamcraft-media --features pipewire --example portal_capture
run() {
  docker run --rm -e RUST_LOG="${RUST_LOG:-info}" -v "${TARGET}/debug/examples/portal_capture:/opt/portal_capture:ro" \
    "${IMAGE}" /opt/portal_capture "$@"
}

echo "== screen (ScreenCast portal, xdg-desktop-portal-wlr, PipeWire)"
run screen --frames 30
# The camera portal grants access and PipeWire links the synthetic camera (a GStreamer
# pipewiresink) to our stream with a negotiated format, but that source never delivers buffers
# in this headless setup, so the frame count is informational; real cameras are covered by
# MANUAL-TEST.md.
echo "== camera (Camera portal; frames informational)"
run camera --frames 10 || echo "camera: no frames from the synthetic source (see run.sh)"
