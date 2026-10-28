#!/bin/sh
# Puts the segmentation model for background effects (#439) into a web client build, under the
# name its index.html gives it (scripts/finish-calls.sh: models/<name>-<pinned hash>.onnx), for the
# web release. The desktop app embeds the same build without it: it bundles the model itself.
#
#   crates/teamcraft-ui/scripts/add-segmentation-model.sh <dist-dir>
#
# The model is fetched and checked against its pinned SHA-256 by crates/teamcraft-media/models/fetch.sh
# (a copy fetched before is reused). Run it before signing the build (sign-web-release.sh lists it).
set -eu
dist="${1:?dist directory}"
models="$(cd "$(dirname "$0")/../../teamcraft-media/models" && pwd)"
[ -f "$dist/index.html" ] || { echo "no index.html in $dist" >&2; exit 1; }
path="$(sed -n 's|.*<meta name="teamcraft-segmentation-model" content="/\([^"]*\)".*|\1|p' "$dist/index.html")"
[ -n "$path" ] || { echo "$dist/index.html names no segmentation model" >&2; exit 1; }
"$models/fetch.sh"
mkdir -p "$dist/$(dirname "$path")"
cp "$models/selfie_segmentation_landscape.onnx" "$dist/$path"
echo "added $path"
