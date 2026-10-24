#!/usr/bin/env python3
"""Renders the notes of a GitHub release from .github/release-notes.md (docs/release.md).

  scripts/render-release-notes.py --version 1.0.0 --tag v1.0.0 --repo owner/name \
      --images dist/images.txt [--generated generated.md] > notes.md

The highlights are the message of the annotated tag (for a candidate tag like v1.0.0-rc.1 with an
empty message: the message of v1.0.0, if that tag exists); its "### Breaking" section, if any, fills
the breaking changes instead. A lightweight or missing tag leaves the highlights empty, and the
generated notes stand alone. The tags must be in the local repository as annotated tag objects.
"""
import argparse
import pathlib
import re
import subprocess
import sys
from typing import Callable

ROOT = pathlib.Path(__file__).resolve().parent.parent
NO_HIGHLIGHTS = "See *What's changed* below."
SIGNATURE = re.compile(r"^-----BEGIN (PGP|SSH) SIGNATURE-----\n.*\Z", re.S | re.M)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--images", required=True)
    parser.add_argument("--generated")
    args = parser.parse_args()

    base_version = args.version.split("-", 1)[0]
    highlights, breaking = split_breaking(release_message(args.tag, base_version, tag_message))
    generated = pathlib.Path(args.generated).read_text().strip() if args.generated else ""
    prerelease = ""
    if "-" in args.version:
        prerelease = (
            f"> **Pre-release** {args.version}: a candidate for {base_version}, for testing the "
            "release. Not for production.\n"
        )

    template = (ROOT / ".github/release-notes.md").read_text()
    template = re.sub(r"\A<!--.*?-->\n", "", template, flags=re.S)
    values = {
        "version": args.version,
        "tag": args.tag,
        "repo": args.repo,
        "prerelease_notice": prerelease,
        "highlights": highlights or NO_HIGHLIGHTS,
        "breaking": breaking or "None.",
        "images": pathlib.Path(args.images).read_text().strip(),
        "generated_notes": generated or "No generated notes.",
    }
    sys.stdout.write(re.sub(r"\{\{(\w+)\}\}", lambda m: values[m.group(1)], template))
    return 0


def release_message(tag: str, base_version: str, lookup: Callable[[str], str]) -> str:
    """The tag's own message; for a candidate with an empty one, the base version's tag message."""
    message = lookup(tag)
    if message or "-" not in tag:
        return message
    return lookup(f"v{base_version}")


def split_breaking(message: str) -> tuple[str, str]:
    """Splits a tag message into the highlights and the body of its "### Breaking" section."""
    match = re.search(r"^### Breaking[^\n]*\n(.*?)(?=^#{1,3} |\Z)", message, re.S | re.M)
    if match is None:
        return message.strip(), ""
    highlights = message[: match.start()] + message[match.end() :]
    return highlights.strip(), match.group(1).strip()


def tag_message(tag: str) -> str:
    """The message of an annotated tag without its signature; "" for a lightweight or missing tag."""
    result = subprocess.run(
        ["git", "for-each-ref", "--format=%(objecttype)%00%(contents)", f"refs/tags/{tag}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    kind, _, contents = result.stdout.partition("\0")
    if kind != "tag":
        return ""
    return strip_signature(contents)


def strip_signature(contents: str) -> str:
    return SIGNATURE.sub("", contents).strip()


if __name__ == "__main__":
    sys.exit(main())
