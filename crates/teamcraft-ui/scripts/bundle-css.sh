#!/bin/sh
# Trunk hook (pre_build): joins the stylesheet's area files (style/main/*.css) into gen/main.css,
# the one stylesheet index.html links (#614). One file and one request as before the split, and
# nothing inline, so the CSP (`style-src 'self'`) is unchanged. The files are joined in name order
# (code point order, whatever the locale): the number in front of a name is its place in the
# cascade. scripts/check-css.py reads them in the same order.
set -eu
here="$(cd "$(dirname "$0")/.." && pwd)"
python="$(command -v python3 || command -v python)"
"$python" - "$here/style/main" "$here/gen/main.css" <<'PY'
import pathlib, sys
source, target = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
parts = sorted(source.glob("*.css"), key=lambda path: path.name)
if not parts:
    raise SystemExit(f"bundle-css: no stylesheets in {source}")
css = b"".join(part.read_bytes() for part in parts)
target.parent.mkdir(exist_ok=True)
# Unchanged content keeps the file (and its time stamp) as it is.
if not target.exists() or target.read_bytes() != css:
    target.write_bytes(css)
PY
