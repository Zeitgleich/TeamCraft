#!/bin/sh
# Signs a web client build (docs/e2ee.md §9.3).
#
#   scripts/sign-web-release.sh <dist-dir> <ed25519-private-key.pem> <version>
#
# Writes <dist-dir>/release.json: every file of the bundle with its SHA-256, and an Ed25519 signature
# over the canonical JSON of {version, files}; and puts the matching public key into <dist-dir>/sw.js.
# `lazy` lists the wasm files index.html and sw.js don't name (the chunks that load when first used
# and the signed-in app's, #334) and the segmentation model (#439): the worker fetches those when
# needed or after a member's page loaded instead of on install, so a guest's meeting page doesn't
# download the signed-in app. It is outside the
# signature (older workers verify {version, files} only); every file is checked against its hash.
# Run it on the machine that holds the release key, not on the server that serves the files.
# Needs openssl 3, python3 and sha256sum (or shasum on macOS).
set -eu
dist="${1:?dist directory}"
key="${2:?private key (PEM)}"
version="${3:?version}"
[ -f "$dist/index.html" ] || { echo "no index.html in $dist" >&2; exit 1; }

public="$(openssl pkey -in "$key" -pubout -outform DER | tail -c 32 | python3 -c 'import base64,sys; print(base64.b64encode(sys.stdin.buffer.read()).decode())')"
python3 - "$dist" "$public" <<'PY'
import sys
path = sys.argv[1] + "/sw.js"
text = open(path).read()
if "__RELEASE_PUBLIC_KEY__" not in text:
    raise SystemExit("sw.js has no key placeholder (already signed?)")
open(path, "w").write(text.replace("__RELEASE_PUBLIC_KEY__", sys.argv[2]))
PY

# sw.js (with its key) and release.json itself are not listed: the worker is fetched by the browser,
# the manifest is what the worker verifies.
python3 - "$dist" "$version" "$key" <<'PY'
import base64, hashlib, json, os, subprocess, sys, tempfile
dist, version, key = sys.argv[1:4]
files = {}
for root, _, names in os.walk(dist):
    for name in names:
        full = os.path.join(root, name)
        rel = os.path.relpath(full, dist).replace(os.sep, "/")
        if rel in ("sw.js", "release.json") or rel.startswith(".") or rel.endswith((".br", ".gz")):
            continue
        files[rel] = hashlib.sha256(open(full, "rb").read()).hexdigest()
canonical = json.dumps({"version": version, "files": files}, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
message = ("teamcraft-web-release-v1\n" + canonical).encode()
with tempfile.NamedTemporaryFile(delete=False) as handle:
    handle.write(message)
signature = subprocess.run(["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", key, "-in", handle.name], check=True, capture_output=True).stdout
os.unlink(handle.name)
entry_points = "".join(open(os.path.join(dist, name), encoding="utf-8").read() for name in ("index.html", "sw.js"))
# The segmentation model (#439) is named in index.html, but only the call module downloads it, when
# the first background effect starts.
lazy = sorted(
    path for path in files
    if path.endswith(".onnx") or (path.endswith(".wasm") and path not in entry_points)
)
manifest = {"version": version, "files": files, "lazy": lazy, "signature": base64.b64encode(signature).decode()}
with open(os.path.join(dist, "release.json"), "w") as out:
    json.dump(manifest, out, indent=1, sort_keys=True)
print(f"signed {len(files)} files ({len(lazy)} loaded when used) as version {version}")
PY
