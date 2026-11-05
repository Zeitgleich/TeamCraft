#!/bin/sh
# Puts the photo backgrounds' full pictures (#439) into a web client build, for the web release:
# assets/backgrounds/<slug>.jpg as backgrounds/<slug>-<hash>.jpg, the hash being the first 16 hex
# digits of the file's SHA-256. The browser's call module downloads one when it is picked and checks
# it against the SHA-256 it pins (teamcraft-media, effects/photo_file.rs); sign-web-release.sh lists
# them as `on_demand`, so the service worker never fetches them in the background. The desktop app
# embeds the same build without them: it compiles the pictures in.
#
#   crates/teamcraft-ui/scripts/add-background-photos.sh <dist-dir>
#
# add-segmentation-model.sh runs it (the release workflow's step for the effects' files). For a local
# `trunk serve`, run it on crates/teamcraft-ui/dist after the build (a rebuild clears it). Run it
# before signing the build.
set -eu
dist="${1:?dist directory}"
photos="$(cd "$(dirname "$0")/../../../assets/backgrounds" && pwd)"
[ -f "$dist/index.html" ] || { echo "no index.html in $dist" >&2; exit 1; }
python3 - "$dist" "$photos" <<'PY'
import hashlib, os, shutil, sys
dist, photos = sys.argv[1], sys.argv[2]
target = os.path.join(dist, "backgrounds")
os.makedirs(target, exist_ok=True)
names = sorted(name for name in os.listdir(photos) if name.endswith(".jpg"))
if not names:
    raise SystemExit(f"no photos in {photos}")
for name in names:
    source = os.path.join(photos, name)
    digest = hashlib.sha256(open(source, "rb").read()).hexdigest()[:16]
    shutil.copyfile(source, os.path.join(target, f"{name[:-len('.jpg')]}-{digest}.jpg"))
print(f"added {len(names)} photo backgrounds to {target}")
PY
