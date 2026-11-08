#!/bin/sh
# Run by build-app.sh (Trunk post_build hook) once init.js exists. Two things from i18n/<language>/*.ftl:
#   - the UI's texts (#1437): each language's files joined into one file of the build,
#     `i18n/<language>-<hash>.ftl`, without comments, as build.rs embeds them for native builds. The
#     web app embeds none: init.js fetches the language the app starts in next to the app, the app
#     the others when the user switches (src/i18n.rs). The administration console's texts (`admin.ftl`)
#     are not in it but in a file of their own, `i18n/<language>-admin-<hash>.ftl`, which the console
#     fetches when it opens (#1462). Their names go into init.js as `globalThis.teamcraftTranslationFiles`
#     (`<language>` and `<language>-admin` -> URL).
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

# The console's messages: a file of their own, only admins load it (#1462).
ADMIN_FILE = "admin.ftl"
texts = {}
joined = {}
admin = {}
# One directory per language, one file per area of the app (#614), each by name.
for path in sorted(glob.glob(os.path.join(i18n, "*", "*.ftl"))):
    language = os.path.basename(os.path.dirname(path))
    source = open(path, encoding="utf-8").read()
    parts = admin if os.path.basename(path) == ADMIN_FILE else joined
    parts[language] = parts.get(language, "") + without_comments(source)
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
# `name` is the file's key in `files`, and with the content's hash the file's name: the server lets
# browsers keep such files for good.
for name, text in [*sorted(joined.items()), *((f"{language}-admin", text) for language, text in sorted(admin.items()))]:
    data = text.encode("utf-8")
    file = f"{name}-{hashlib.sha256(data).hexdigest()[:16]}.ftl"
    with open(os.path.join(dist, "i18n", file), "wb") as out:
        out.write(data)
    files[name] = f"/i18n/{file}"
# Imports are hoisted, so this runs before the initializer is called, wherever it stands.
lines = (
    "globalThis.teamcraftLoaderTexts = " + json.dumps(texts, ensure_ascii=True, sort_keys=True) + ";\n"
    + "globalThis.teamcraftTranslationFiles = " + json.dumps(files, sort_keys=True) + ";\n"
)
open(init_path, "w", encoding="utf-8").write(lines + init)
PY
