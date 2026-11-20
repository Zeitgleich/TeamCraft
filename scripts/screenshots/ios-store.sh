#!/usr/bin/env bash
# App Store screenshots of the iOS app, into <output dir>/<locale>/<device>/: the
# scenes of scripts/screenshots/ios-store/scenes.js (the Play screenshots' scenes and placeholder
# texts, drawn in the iOS app's light theme with a caption above the device) taken in headless
# Chrome by ios-store.mjs, at iPhone 6.9" (1320 x 2868) and iPad 13" (2064 x 2752). The iPhone
# 6.3" pictures (1206 x 2622), the size App Store Connect requires, are the 6.9" ones scaled down:
# both screens have the same layout in points, and their aspect ratios differ by less than 0.1 %.
#
#   scripts/screenshots/ios-store.sh <output dir>
#
# The pictures are not kept in the repository: put the output dir outside the checkout, upload
# from there and throw it away.
#
# Needs Node 22+, Chrome (CHROME=/path/to/chrome if it is not google-chrome), python3 with Pillow,
# and the fonts Inter and Noto Color Emoji. Upload: apps/ios/scripts/upload-screenshots.py.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="${1:?usage: ios-store.sh <output dir>}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

node "$root/scripts/screenshots/ios-store.mjs" "$work" >/dev/null

# Screenshots as JPEG (App Store Connect takes .jpg and .png): about half the size of PNG here.
# Quality 92 without chroma subsampling keeps text and line icons free of colour fringes.
python3 - "$work" "$out" <<'PY'
import shutil
import sys
from pathlib import Path
from PIL import Image

LOCALES = {"en": "en-US", "de": "de-DE"}
IPHONE_63 = (1206, 2622)
CREATIVE = {"header", "search"}
work, out_dir = Path(sys.argv[1]), Path(sys.argv[2])
for language, locale in LOCALES.items():
  target = out_dir / locale
  shutil.rmtree(target, ignore_errors=True)
  for shot in sorted((work / language).glob("*/*.png")):
    # The header and search result assets take no JPEG (App Store Connect: invalid extension).
    is_creative = shot.parent.name in CREATIVE
    out = target / shot.parent.name / f"{shot.stem}.{'png' if is_creative else 'jpg'}"
    out.parent.mkdir(parents=True, exist_ok=True)
    image = Image.open(shot).convert("RGB")
    if is_creative:
      image.save(out, optimize=True)
    else:
      image.save(out, "JPEG", quality=92, subsampling=0, optimize=True)
    print(out)
    if shot.parent.name == "iphone-6.9":
      medium = target / "iphone-6.3" / out.name
      medium.parent.mkdir(parents=True, exist_ok=True)
      image.resize(IPHONE_63, Image.Resampling.LANCZOS).save(medium, "JPEG", quality=92, subsampling=0, optimize=True)
      print(medium)
PY
