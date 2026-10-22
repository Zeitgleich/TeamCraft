#!/usr/bin/env python3
"""Renders the notes of a GitHub release from .github/release-notes.md (docs/release.md).

  scripts/render-release-notes.py --version 1.0.0 --tag v1.0.0 --repo owner/name \
      --images dist/images.txt [--generated generated.md] > notes.md

The changelog part is the CHANGELOG.md section of the version (for a candidate tag like v1.0.0-rc.1
the section of 1.0.0); its "### Breaking" subsection, if any, also fills the breaking changes.
"""
import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--images", required=True)
    parser.add_argument("--generated")
    args = parser.parse_args()

    base_version = args.version.split("-", 1)[0] if "-" in args.version else args.version
    changelog = changelog_section(args.version) or changelog_section(base_version)
    if changelog is None:
        print(f"CHANGELOG.md has no section for {args.version} or {base_version}", file=sys.stderr)
        return 1
    breaking = subsection(changelog, "Breaking") or "None."
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
        "changelog": changelog.strip(),
        "breaking": breaking.strip(),
        "images": pathlib.Path(args.images).read_text().strip(),
        "generated_notes": generated or "See the changelog above.",
    }
    sys.stdout.write(re.sub(r"\{\{(\w+)\}\}", lambda m: values[m.group(1)], template))
    return 0


def changelog_section(version: str) -> str | None:
    text = (ROOT / "CHANGELOG.md").read_text()
    match = re.search(rf"^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## \[|^\[[^\]]+\]: )", text, re.S | re.M)
    return match.group(1) if match else None


def subsection(section: str, title: str) -> str | None:
    match = re.search(rf"^### {title}[^\n]*\n(.*?)(?=^### |\Z)", section, re.S | re.M)
    return match.group(1) if match else None


if __name__ == "__main__":
    sys.exit(main())
