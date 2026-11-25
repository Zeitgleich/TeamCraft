#!/usr/bin/env python3
"""Minifies index.html of a release build (#2396): drops the developer comments (but for the TeamCraft banner) and the whitespace
between tags, and collapses the rest. Run last in build-app.sh, after every hook that edits the
page (finish-calls.sh looks for `</head>`) and before the web release is signed (the hashes cover
the minified file). The page has no inline script, style or preformatted text (the CSP forbids the
first two); the script refuses a page that gains one, because collapsing whitespace there would
change its meaning."""
import re
import sys

# The one comment the release keeps, right after the doctype. Plain ASCII, and no two hyphens in a
# row (they would end a comment early).
BANNER = r"""
 _____                      ____            __ _
|_   _|__  __ _ _ __ ___   / ___|_ __ __ _ / _| |_
  | |/ _ \/ _` | '_ ` _ \ | |   | '__/ _` | |_| __|
  | |  __/ (_| | | | | | || |___| | | (_| |  _| |_
  |_|\___|\__,_|_| |_| |_| \____|_|  \__,_|_|  \__|
"""

# Whitespace is meaningful inside these; the page has none of them with content.
SENSITIVE = re.compile(r"<(script|style|pre|textarea)\b[^>]*>(.+?)</\1\s*>", re.S | re.I)


def minify(page):
    page = re.sub(r"<!--.*?-->", "", page, flags=re.S)
    for match in SENSITIVE.finditer(page):
        if match.group(2).strip():
            raise SystemExit(f"minify-html: <{match.group(1)}> has content; not safe to collapse")
    page = re.sub(r"\s+", " ", page)
    page = re.sub(r">\s+<", "><", page)
    page = page.strip()
    doctype = re.match(r"<!doctype[^>]*>", page, re.I)
    if not doctype or "--" in BANNER:
        raise SystemExit("minify-html: no doctype to put the banner after")
    return page[: doctype.end()] + "<!--" + BANNER + "-->" + page[doctype.end() :] + "\n"


if __name__ == "__main__":
    path = sys.argv[1]
    with open(path, encoding="utf-8") as source:
        original = source.read()
    with open(path, "w", encoding="utf-8") as target:
        target.write(minify(original))
