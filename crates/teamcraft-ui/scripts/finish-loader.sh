#!/bin/sh
# Run by externalize-init.sh (Trunk post_build hook) once init.js exists: puts the loading screen's
# texts (the `loader-*` messages of every i18n/*.ftl) into init.js as `globalThis.teamcraftLoaderTexts`,
# where loader.js picks them by language. Not into loader.js itself: index.html pins that file by its
# integrity value.
set -eu
dist="${TRUNK_STAGING_DIR:?run this as a Trunk hook}"
# Git Bash on Windows gets the staging directory as \\?\D:\...; use the mixed form (D:/...) that awk,
# python and the shell all accept.
case "$dist" in
  [A-Za-z]:*|'\\'*) dist=$(cygpath -m "${dist#'\\?\'}") ;;
esac
here="$(cd "$(dirname "$0")/.." && pwd)"
python="$(command -v python3 || command -v python)"
"$python" - "$dist/init.js" "$here/i18n" <<'PY'
import glob, json, os, re, sys
init_path, i18n = sys.argv[1:3]
texts = {}
for path in sorted(glob.glob(os.path.join(i18n, "*.ftl"))):
    language = os.path.splitext(os.path.basename(path))[0]
    messages = {}
    for line in open(path, encoding="utf-8"):
        match = re.match(r"^(loader-[a-z-]+)\s*=\s*(.+?)\s*$", line)
        if match:
            messages[match.group(1)] = match.group(2)
    if messages:
        texts[language] = messages
if "en" not in texts:
    raise SystemExit("finish-loader: i18n/en.ftl has no loader-* messages")
init = open(init_path, encoding="utf-8").read()
if "teamcraftLoaderTexts" in init:
    raise SystemExit("finish-loader: init.js already has the texts")
# Imports are hoisted, so this runs before the initializer is called, wherever it stands.
line = "globalThis.teamcraftLoaderTexts = " + json.dumps(texts, ensure_ascii=True, sort_keys=True) + ";\n"
open(init_path, "w", encoding="utf-8").write(line + init)
PY
