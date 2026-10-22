#!/bin/sh
# Trunk hook (post_build): moves the inline module script of index.html into init.js, so the page
# works under the server's default Content-Security-Policy (script-src 'self', no inline scripts).
set -eu
index="${TRUNK_STAGING_DIR:?run this as a Trunk hook}/index.html"
init="${TRUNK_STAGING_DIR}/init.js"
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
