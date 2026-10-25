#!/usr/bin/env python3
"""Writes the Google Play "What's new" files of a release (docs/release.md, "Google Play").

  scripts/play-release-notes.py --version 1.0.0 --tag v1.0.0 --repo owner/name \
      --locales en-US,de-DE --out-dir whatsnew

The text is the highlights of the annotated tag, as scripts/render-release-notes.py finds them
(without the "### Breaking" section, which is for operators), as plain text: Play shows no Markdown.
Play allows 500 characters per language; longer highlights are cut at a line or word and end with a
link to the full notes. The highlights are English; every locale gets the same text, one file
`whatsnew-<locale>` each (the format of r0adkll/upload-google-play).
"""
import argparse
import importlib.util
import pathlib
import re
import sys

SCRIPTS = pathlib.Path(__file__).resolve().parent
# Play's limit per language. Counted in UTF-16 code units (Play is Java), so an emoji counts twice.
MAX_LENGTH = 500
ELLIPSIS = "…"

_SPEC = importlib.util.spec_from_file_location("render_release_notes", SCRIPTS / "render-release-notes.py")
render = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(render)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--locales", required=True, help="comma-separated BCP 47 tags, e.g. en-US,de-DE")
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()

    base_version = args.version.split("-", 1)[0]
    highlights, _breaking = render.split_breaking(
        render.release_message(args.tag, base_version, render.tag_message)
    )
    url = f"https://github.com/{args.repo}/releases/tag/{args.tag}"
    text = play_notes(args.version, highlights, url)

    out_dir = pathlib.Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    for locale in filter(None, (part.strip() for part in args.locales.split(","))):
        (out_dir / f"whatsnew-{locale}").write_text(text)
    sys.stdout.write(text + "\n")
    return 0


def play_notes(version: str, highlights: str, url: str) -> str:
    """The plain-text notes for Play: a candidate notice, the highlights, at most MAX_LENGTH long."""
    lines = []
    if "-" in version:
        lines.append(f"Release candidate {version}, for testing.")
    lines.append(plain_text(highlights) or f"TeamCraft {version}")
    return truncate("\n\n".join(lines), url)


def plain_text(markdown: str) -> str:
    """Markdown as plain text: headings, emphasis, code and links reduced to their text."""
    text = re.sub(r"!\[[^\]]*\]\([^)]*\)", "", markdown)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"^#{1,6}\s*", "", text, flags=re.M)
    text = re.sub(r"^(\s*)[-*+]\s+", r"\1• ", text, flags=re.M)
    text = re.sub(r"(\*\*|__|`)", "", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return "\n".join(line.rstrip() for line in text.strip().splitlines())


def truncate(text: str, url: str) -> str:
    """The text if it fits; else cut at a line (or word) break, followed by "…" and the link."""
    if length(text) <= MAX_LENGTH:
        return text
    suffix = f"\n{ELLIPSIS} {url}"
    budget = MAX_LENGTH - length(suffix)
    if budget <= 0:
        # A link that long leaves no room; the cut text alone still fits.
        suffix, budget = ELLIPSIS, MAX_LENGTH - length(ELLIPSIS)
    cut = fit(text, budget)
    for separator in ("\n", " "):
        position = cut.rfind(separator)
        # Only break at a line or word if that keeps at least half of the room.
        if position > 0 and length(cut[:position]) >= budget // 2:
            cut = cut[:position]
            break
    return cut.rstrip() + suffix


def fit(text: str, budget: int) -> str:
    """The longest prefix of the text within the budget, never splitting a character."""
    used = 0
    for index, char in enumerate(text):
        used += length(char)
        if used > budget:
            return text[:index]
    return text


def length(text: str) -> int:
    return len(text.encode("utf-16-le")) // 2


if __name__ == "__main__":
    sys.exit(main())
