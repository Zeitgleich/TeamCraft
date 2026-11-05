#!/usr/bin/env bash
# The segmentation model for the browser's background effects (#439, #1295): the ONNX model
# (fetch.sh, which this runs first) converted to NNEF, tract's own format, which the browser's
# segmentation worker reads without tract's ONNX parser (most of that module's size). Usage:
#
#   crates/teamcraft-media/models/fetch-nnef.sh [--pin] [destination directory]
#
# Default destination: this directory. Needs cargo: the conversion is the `segmentation_model_nnef`
# example of teamcraft-media. It gives the same archive for the same model and tract version (fixed
# timestamps, sorted entries), so the result is pinned like the ONNX file
# (selfie_segmentation_landscape.nnef.sha256); the worker module checks the same pin. --pin writes
# the conversion's hash as the new pin instead of checking it: after a tract upgrade changed the
# archive (the unit test `the_nnef_archive_is_the_same_every_time` says so), once
# `cargo test -p teamcraft-media --features native,segmentation --test suite segmentation_model`
# showed that it segments like the ONNX model.
set -euo pipefail

MODELS_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "${MODELS_DIR}/../../.." && pwd)"
is_pinning=0
if [ "${1:-}" = "--pin" ]; then
  is_pinning=1
  shift
fi
DEST="${1:-${MODELS_DIR}}"
NAME=selfie_segmentation_landscape.nnef
ONNX="${MODELS_DIR}/selfie_segmentation_landscape.onnx"

# sha256sum on Linux, shasum on macOS.
sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1
}

expected="$(cut -d ' ' -f 1 "${MODELS_DIR}/${NAME}.sha256")"
mkdir -p "${DEST}"
if [ "${is_pinning}" = 0 ] && [ -f "${DEST}/${NAME}" ] && [ "$(sha256 "${DEST}/${NAME}")" = "${expected}" ]; then
  echo "${DEST}/${NAME} is up to date"
  exit 0
fi

"${MODELS_DIR}/fetch.sh"
partial="$(mktemp "${DEST}/${NAME}.XXXXXX")"
trap 'rm -f "${partial}"' EXIT
pin_flag=()
[ "${is_pinning}" = 1 ] && pin_flag=(--pin)
cargo run --quiet --release --locked --manifest-path "${ROOT}/Cargo.toml" -p teamcraft-media \
  --no-default-features --features segmentation --example segmentation_model_nnef -- \
  "${ONNX}" "${partial}" ${pin_flag[@]+"${pin_flag[@]}"}
actual="$(sha256 "${partial}")"
if [ "${is_pinning}" = 1 ]; then
  echo "${actual}  ${NAME}" > "${MODELS_DIR}/${NAME}.sha256"
  echo "pinned ${NAME}: ${actual}"
elif [ "${actual}" != "${expected}" ]; then
  echo "checksum mismatch for the converted ${NAME}: expected ${expected}, got ${actual}" >&2
  exit 1
fi
chmod 644 "${partial}"
mv "${partial}" "${DEST}/${NAME}"
trap - EXIT
echo "converted ${DEST}/${NAME}"
