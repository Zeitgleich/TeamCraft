#!/bin/sh
# Run by build-app.sh (Trunk post_build hook): gives the call module (#330) and its AudioWorklet
# content-hashed names, so the server caches them for good and a page never loads another build's
# module (Trunk names its worker assets without a hash), and tells the page the names in
# <meta name="teamcraft-calls"> and <meta name="teamcraft-call-worklet"> (web_calls.rs).
set -eu
dist="${TRUNK_STAGING_DIR:?run this as a Trunk hook}"
case "$dist" in
  [A-Za-z]:*|'\\'*) dist=$(cygpath -m "${dist#'\\?\'}") ;;
esac
python="$(command -v python3 || command -v python)"
"$python" - "$dist" <<'PY'
import hashlib, os, sys
dist = sys.argv[1]

def hashed(name, stem, suffix):
    path = os.path.join(dist, name)
    digest = hashlib.sha256(open(path, "rb").read()).hexdigest()[:16]
    new = f"{stem}-{digest}{suffix}"
    os.replace(path, os.path.join(dist, new))
    return new

wasm = hashed("teamcraft-web-calls_bg.wasm", "teamcraft-web-calls", "_bg.wasm")
glue_path = os.path.join(dist, "teamcraft-web-calls.js")
glue = open(glue_path, encoding="utf-8").read()
if "'teamcraft-web-calls_bg.wasm'" not in glue:
    raise SystemExit("finish-calls: the call module's glue doesn't name its wasm")
open(glue_path, "w", encoding="utf-8").write(glue.replace("'teamcraft-web-calls_bg.wasm'", f"'{wasm}'"))
module = hashed("teamcraft-web-calls.js", "teamcraft-web-calls", ".js")
worklet = hashed("teamcraft-call-worklet.js", "teamcraft-call-worklet", ".js")

index_path = os.path.join(dist, "index.html")
index = open(index_path, encoding="utf-8").read()
if "</head>" not in index:
    raise SystemExit("finish-calls: index.html has no </head>")
meta = (f'<meta name="teamcraft-calls" content="/{module}">\n'
        f'<meta name="teamcraft-call-worklet" content="/{worklet}">\n')
open(index_path, "w", encoding="utf-8").write(index.replace("</head>", meta + "</head>", 1))
PY
