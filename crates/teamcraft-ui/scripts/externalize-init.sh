#!/bin/sh
# Trunk hook (post_build): moves the inline module script of index.html into init.js, so the page
# works under the server's default Content-Security-Policy (script-src 'self', no inline scripts).
set -eu
staging="${TRUNK_STAGING_DIR:?run this as a Trunk hook}"
# Git Bash on Windows gets the staging directory as \\?\D:\...; use the mixed form (D:/...) that awk,
# python and the shell all accept.
case "$staging" in
  [A-Za-z]:*|'\\'*) staging=$(cygpath -m "${staging#'\\?\'}") ;;
esac
index="$staging/index.html"
init="$staging/init.js"
awk -v init="$init" '
  /<script type="module">/ && !done { inside = 1; sub(/.*<script type="module">/, ""); }
  inside {
    line = $0
    if (line ~ /<\/script>/) { sub(/<\/script>.*/, "", line); inside = 0; done = 1; print line >> init; print "<script type=\"module\" src=\"/init.js\"></script>"; next }
    print line >> init
    next
  }
  { print }
' "$index" > "$index.tmp"
mv "$index.tmp" "$index"
# The loading screen's initializer is now in init.js (it needs this file to exist).
sh "$(dirname "$0")/finish-loader.sh"
