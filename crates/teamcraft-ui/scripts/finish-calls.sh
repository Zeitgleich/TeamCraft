#!/bin/sh
# Run by build-app.sh (Trunk post_build hook): gives the call module (#330) and its AudioWorklet
# content-hashed names, so the server caches them for good and a page never loads another build's
# module (Trunk names its worker assets without a hash), and tells the page the names in
# <meta name="teamcraft-calls"> and <meta name="teamcraft-call-worklet"> (web_calls.rs).
#
# The segmentation model for background effects (#439) is named in
# <meta name="teamcraft-segmentation-model"> under a name with its pinned hash
# (models/<name>-<hash>.onnx); the call module downloads it when the first effect starts. The file
# is copied there when crates/teamcraft-media/models/fetch.sh fetched it; the web release adds it
# with add-segmentation-model.sh instead (the desktop app embeds this bundle and has its own copy).
# Without it the browser's effects say they couldn't start.
set -eu
dist="${TRUNK_STAGING_DIR:?run this as a Trunk hook}"
case "$dist" in
  [A-Za-z]:*|'\\'*) dist=$(cygpath -m "${dist#'\\?\'}") ;;
esac
# Git Bash on Windows: the mixed form, as for the staging directory.
models="$(cd "$(dirname "$0")/../../teamcraft-media/models" && (pwd -W 2>/dev/null || pwd))"
python="$(command -v python3 || command -v python)"
"$python" - "$dist" "$models" <<'PY'
import hashlib, os, shutil, sys
dist, models = sys.argv[1], sys.argv[2]

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

model_name = "selfie_segmentation_landscape"
model_path = os.path.join(models, model_name + ".onnx")
pinned = open(model_path + ".sha256", encoding="utf-8").read().split()[0]
model = f"models/{model_name}-{pinned[:16]}.onnx"
meta += f'<meta name="teamcraft-segmentation-model" content="/{model}">\n'
if os.path.isfile(model_path):
    if hashlib.sha256(open(model_path, "rb").read()).hexdigest() != pinned:
        raise SystemExit(f"finish-calls: {model_path} is not the pinned model (run models/fetch.sh)")
    os.makedirs(os.path.join(dist, "models"), exist_ok=True)
    shutil.copyfile(model_path, os.path.join(dist, model))
open(index_path, "w", encoding="utf-8").write(index.replace("</head>", meta + "</head>", 1))
PY
