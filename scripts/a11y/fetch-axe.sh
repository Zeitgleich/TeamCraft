#!/usr/bin/env bash
# Downloads the pinned axe-core release (issue #2287) from the npm registry, checks its integrity
# and writes axe.min.js to the given directory. The version and the sha512 live here, nowhere else:
# a new version is a reviewed change of these two lines.
#
#   scripts/a11y/fetch-axe.sh <output dir>
set -euo pipefail

AXE_VERSION="4.10.3"
AXE_INTEGRITY="sha512-Xm7bpRXnDSX2YE2YFfBk2FnF0ep6tmG7xPh8iHee8MIcrgq762Nkce856dYtJYLkuIoYZvGfTs/PbZhideTcEg=="

out="${1:?usage: fetch-axe.sh <output dir>}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

curl --fail --silent --show-error --location --retry 3 \
  "https://registry.npmjs.org/axe-core/-/axe-core-${AXE_VERSION}.tgz" -o "$work/axe-core.tgz"
actual="sha512-$(openssl dgst -sha512 -binary "$work/axe-core.tgz" | openssl base64 -A)"
if [[ "$actual" != "$AXE_INTEGRITY" ]]; then
  echo "axe-core ${AXE_VERSION}: integrity mismatch ($actual)" >&2
  exit 1
fi
tar -xzf "$work/axe-core.tgz" -C "$work" package/axe.min.js
cp "$work/package/axe.min.js" "$out/axe.min.js"
echo "axe-core ${AXE_VERSION}: $out/axe.min.js"
