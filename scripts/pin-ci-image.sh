#!/usr/bin/env bash
# Pins the CI image digest in the workflows: scripts/pin-ci-image.sh sha256:<digest>
# The digest is printed by the "CI image" workflow (.github/workflows/ci-image.yml).
set -euo pipefail

digest="${1:?usage: pin-ci-image.sh sha256:<digest>}"
case "$digest" in
  sha256:????????????????????????????????????????????????????????????????) ;;
  *) echo "not a sha256 digest: $digest" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
for file in .github/workflows/*.yml; do
  # Portable in-place edit (GNU and BSD sed differ).
  sed "s#ghcr.io/zeitgleich/teamcraft-ci@sha256:[0-9a-f]*#ghcr.io/zeitgleich/teamcraft-ci@${digest}#g" \
    "$file" > "$file.tmp"
  mv "$file.tmp" "$file"
done
echo "pinned ghcr.io/zeitgleich/teamcraft-ci@${digest}"
