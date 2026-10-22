#!/usr/bin/env bash
# Downloads the person segmentation model for background effects at packaging time (it is not
# kept in git) and checks it against the pinned SHA-256. Usage:
#
#   crates/teamcraft-media/models/fetch.sh [destination directory]
#
# Default destination: this directory. The model is MediaPipe's selfie segmentation (landscape,
# 256x144) as ONNX, Apache-2.0; see NOTICE.md. Pinned to a repository revision, so the URL keeps
# serving the same file; the checksum guards against anything else.
set -euo pipefail

MODELS_DIR="$(cd "$(dirname "$0")" && pwd)"
DEST="${1:-${MODELS_DIR}}"
NAME=selfie_segmentation_landscape.onnx
REVISION=2497d5bec26c626c7b3c4edc6e1fefc21b64f6c3
URL="https://huggingface.co/onnx-community/mediapipe_selfie_segmentation_landscape/resolve/${REVISION}/onnx/model.onnx"

# sha256sum on Linux, shasum on macOS.
sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1
}

expected="$(cut -d ' ' -f 1 "${MODELS_DIR}/${NAME}.sha256")"
mkdir -p "${DEST}"
if [ -f "${DEST}/${NAME}" ] && [ "$(sha256 "${DEST}/${NAME}")" = "${expected}" ]; then
  echo "${DEST}/${NAME} is up to date"
  exit 0
fi

partial="$(mktemp "${DEST}/${NAME}.XXXXXX")"
trap 'rm -f "${partial}"' EXIT
curl --fail --location --silent --show-error --max-filesize 16777216 --output "${partial}" "${URL}"
actual="$(sha256 "${partial}")"
if [ "${actual}" != "${expected}" ]; then
  echo "checksum mismatch for ${URL}: expected ${expected}, got ${actual}" >&2
  exit 1
fi
chmod 644 "${partial}"
mv "${partial}" "${DEST}/${NAME}"
trap - EXIT
echo "fetched ${DEST}/${NAME}"
