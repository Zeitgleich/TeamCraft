#!/usr/bin/env bash
# Fails when the desktop build embedded the placeholder UI that the release jobs compile against
# while the real bundle is still being built (.github/actions/await-ui-bundle).
#
#   scripts/check-embedded-ui.sh [TARGET_DIR]
#
# apps/desktop/build/frontend.rs copies the bundle into the build script's output directory
# (`out/frontend`); the newest copy is the one the last build embedded.
set -euo pipefail

target_dir="${1:-target}"
index="$(find "$target_dir" -path '*/build/teamcraft-desktop-*/out/frontend/index.html' -type f -print0 \
  | xargs -0 ls -t 2>/dev/null | head -n 1)"
if [ -z "$index" ]; then
  echo "error: no embedded index.html found under $target_dir" >&2
  exit 1
fi
if grep -q 'teamcraft-ui-build-pending' "$index"; then
  echo "error: the build embeds the placeholder UI ($index)" >&2
  exit 1
fi
echo "ok: the build embeds the real UI ($index)"
