#!/bin/sh
# Puts the segmentation model for background effects (#439) into a web client build, under the
# name its index.html gives it (scripts/finish-calls.sh: models/<name>-<pinned hash>.nnef), for the
# web release. The desktop app embeds the same build without it: it bundles the ONNX model itself.
#
#   crates/teamcraft-ui/scripts/add-segmentation-model.sh <dist-dir>
#
# The browser's model is the ONNX model as NNEF (#1295): crates/teamcraft-media/models/fetch-nnef.sh
# fetches the ONNX file, converts it (with cargo) and checks the result against its pinned SHA-256
# (a copy made before is reused). Run it before signing the build (sign-web-release.sh lists it).
# It adds the photo backgrounds' full pictures too (add-background-photos.sh): the release workflow
# runs this script for every web release-only file of the effects.
set -eu
dist="${1:?dist directory}"
models="$(cd "$(dirname "$0")/../../teamcraft-media/models" && pwd)"
[ -f "$dist/index.html" ] || { echo "no index.html in $dist" >&2; exit 1; }
path="$(sed -n 's|.*<meta name="teamcraft-segmentation-model" content="/\([^"]*\)".*|\1|p' "$dist/index.html")"
[ -n "$path" ] || { echo "$dist/index.html names no segmentation model" >&2; exit 1; }
"$models/fetch-nnef.sh"
mkdir -p "$dist/$(dirname "$path")"
cp "$models/selfie_segmentation_landscape.nnef" "$dist/$path"
echo "added $path"
"$(dirname "$0")/add-background-photos.sh" "$dist"
