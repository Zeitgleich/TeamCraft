#!/usr/bin/env bash
# Google Play listing graphics for the Android app (issue #479), into apps/android/store in the
# layout of fastlane supply's metadata directory (`<locale>/images/phoneScreenshots/…`):
#
# 1. the phone screenshots: StoreScreenshots (androidTest, placeholder data, never in a shipped
#    build) on a headless Pixel 6 emulator (1080 × 2400) in the Android build image, SystemUI demo
#    mode for a clean status bar, once per language;
# 2. the feature graphic (scripts/screenshots/feature_graphic.py, Inkscape);
# 3. the hi-res icon (scripts/brand/icons.py).
#
#   TEAMCRAFT_ANDROID_CACHE=/big/disk/android scripts/screenshots/android-store.sh
#
# TEAMCRAFT_ANDROID_CACHE holds the SDK, the emulator, its system image and the AVD (several GB;
# apps/android/README.md). Needs Docker with /dev/kvm, python3 with Pillow, Inkscape and Inter.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cache="${TEAMCRAFT_ANDROID_CACHE:?set TEAMCRAFT_ANDROID_CACHE to a directory on a big disk}"
store="$root/apps/android/store"
# Store locale per language of StoreScreenshots.
locales=(en:en-US de:de-DE)

# Only the emulator's ABI: the screenshots don't need the phone library.
TEAMCRAFT_ANDROID_ABIS=x86_64 "$root/apps/android/scripts/docker-build.sh" \
  --store-screenshots /android/store-screenshots

for pair in "${locales[@]}"; do
  language="${pair%%:*}"
  locale="${pair#*:}"
  target="$store/$locale/images/phoneScreenshots"
  rm -rf "$target"
  mkdir -p "$target"
  # Play takes no alpha channel; the pictures become RGB PNGs.
  python3 - "$cache/store-screenshots/$language" "$target" <<'PY'
import sys
from pathlib import Path
from PIL import Image

source, target = Path(sys.argv[1]), Path(sys.argv[2])
shots = sorted(source.glob("*.png"))
if not shots:
  sys.exit(f"no screenshots in {source}")
for shot in shots:
  Image.open(shot).convert("RGB").save(target / shot.name, optimize=True)
  print(target / shot.name)
PY
done

python3 "$root/scripts/screenshots/feature_graphic.py"
python3 "$root/scripts/brand/icons.py" >/dev/null
