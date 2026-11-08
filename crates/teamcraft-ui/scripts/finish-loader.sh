#!/bin/sh
# Run by build-app.sh (Trunk post_build hook) once init.js exists. Two things from i18n/<language>/*.ftl:
#   - the UI's texts (#1437): each language's files joined into one file of the build,
#     `i18n/<language>-<hash>.ftl`, without comments, as build.rs embeds them for native builds. The
#     web app embeds none: init.js fetches the language the app starts in next to the app, the app
#     the others when the user switches (src/i18n.rs). Their names go into init.js as
#     `globalThis.teamcraftTranslationFiles` (language -> URL).
#   - the loading screen's texts (the `loader-*` messages), into init.js as
#     `globalThis.teamcraftLoaderTexts`, where loader.js picks them by language. Not into loader.js
#     itself: index.html pins that file by its integrity value.
set -eu
dist="${TRUNK_STAGING_DIR:?run this as a Trunk hook}"
# Git Bash on Windows gets the staging directory as \\?\D:\...; use the mixed form (D:/...) that awk,
# python and the shell all accept.
case "$dist" in
  [A-Za-z]:*|'\\'*) dist=$(cygpath -m "${dist#'\\?\'}") ;;
esac
here="$(cd "$(dirname "$0")/.." && pwd)"
python="$(command -v python3 || command -v python)"
"$python" - "$dist" "$here/i18n" <<'PY'
import glob, hashlib, json, os, re, sys
dist, i18n = sys.argv[1:3]
init_path = os.path.join(dist, "init.js")

def without_comments(source):
    """As build.rs `without_comments`: every line but those that start with `#` (a message's
    continuation lines are indented), each ended by a newline."""
    lines = source.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    lines = [line[:-1] if line.endswith("\r") else line for line in lines]
    return "".join(line + "\n" for line in lines if not line.startswith("#"))

texts = {}
joined = {}
# One directory per language, one file per area of the app (#614), each by name.
for path in sorted(glob.glob(os.path.join(i18n, "*", "*.ftl"))):
    language = os.path.basename(os.path.dirname(path))
    source = open(path, encoding="utf-8").read()
    joined[language] = joined.get(language, "") + without_comments(source)
    for line in source.splitlines():
        match = re.match(r"^(loader-[a-z-]+)\s*=\s*(.+?)\s*$", line)
        if match:
            texts.setdefault(language, {})[match.group(1)] = match.group(2)
if "en" not in texts:
    raise SystemExit("finish-loader: i18n/en has no loader-* messages")
init = open(init_path, encoding="utf-8").read()
if "teamcraftLoaderTexts" in init:
    raise SystemExit("finish-loader: init.js already has the texts")
files = {}
os.makedirs(os.path.join(dist, "i18n"), exist_ok=True)
for language, text in sorted(joined.items()):
    data = text.encode("utf-8")
    # The content in the name: the server lets browsers keep such files for good.
    name = f"{language}-{hashlib.sha256(data).hexdigest()[:16]}.ftl"
    with open(os.path.join(dist, "i18n", name), "wb") as out:
        out.write(data)
    files[language] = f"/i18n/{name}"
# Imports are hoisted, so this runs before the initializer is called, wherever it stands.
lines = (
    "globalThis.teamcraftLoaderTexts = " + json.dumps(texts, ensure_ascii=True, sort_keys=True) + ";\n"
    + "globalThis.teamcraftTranslationFiles = " + json.dumps(files, sort_keys=True) + ";\n"
)
open(init_path, "w", encoding="utf-8").write(lines + init)
PY
