#!/usr/bin/env bash
# App Store screenshots of the iOS app, into apps/ios/store/<locale>/screenshots/<device>/: the
# scenes of scripts/screenshots/ios-store/scenes.js (the Play screenshots' scenes and placeholder
# texts, drawn in the iOS app's light theme with a caption above the device) taken in headless
# Chrome by ios-store.mjs, at iPhone 6.9" (1320 x 2868) and iPad 13" (2064 x 2752).
#
#   scripts/screenshots/ios-store.sh
#
# Needs Node 22+, Chrome (CHROME=/path/to/chrome if it is not google-chrome), python3 with Pillow,
# and the fonts Inter and Noto Color Emoji. Upload: apps/ios/scripts/upload-screenshots.py.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
store="$root/apps/ios/store"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

node "$root/scripts/screenshots/ios-store.mjs" "$work" >/dev/null

# App Store Connect takes RGB without transparency; store locale per language.
python3 - "$work" "$store" <<'PY'
import shutil
import sys
from pathlib import Path
from PIL import Image

LOCALES = {"en": "en-US", "de": "de-DE"}
work, store = Path(sys.argv[1]), Path(sys.argv[2])
for language, locale in LOCALES.items():
  target = store / locale / "screenshots"
  shutil.rmtree(target, ignore_errors=True)
  for shot in sorted((work / language).glob("*/*.png")):
    out = target / shot.parent.name / shot.name
    out.parent.mkdir(parents=True, exist_ok=True)
    Image.open(shot).convert("RGB").save(out, optimize=True)
    print(out.relative_to(store.parents[2]))
PY
