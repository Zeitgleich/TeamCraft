#!/usr/bin/env python3
"""Merges `sha256sum` lists into a release's SHA256SUMS (docs/release.md, "Checksums").

  scripts/merge-checksums.py <existing> <new> > SHA256SUMS

The release workflow attaches its files in two steps: the draft release job (desktop, web, server)
and the Android job when that is done, each with the files it built. The result holds every line of
<new> and the lines of <existing> for files <new> doesn't list, so either step can run (or rerun)
first without dropping the other's lines. A missing <existing> counts as empty. Lines are sorted by
file name, so the result doesn't depend on the order of the steps.
"""
import pathlib
import re
import sys

# `sha256sum` output: 64 hex digits, a space, ` ` (text) or `*` (binary), the file name.
LINE = re.compile(r"^([0-9a-f]{64}) ([ *])(.+)$")


class ChecksumError(Exception):
    pass


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        existing = read(pathlib.Path(argv[1]), may_be_missing=True)
        new = read(pathlib.Path(argv[2]), may_be_missing=False)
    except ChecksumError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    sys.stdout.write(render(merge(existing, new)))
    return 0


def merge(existing, new):
    """{name: line} of both, the new line winning for a file listed in both."""
    merged = dict(existing)
    merged.update(new)
    return merged


def render(entries):
    return "".join(f"{entries[name]}\n" for name in sorted(entries))


def read(path, may_be_missing):
    if not path.exists():
        if may_be_missing:
            return {}
        raise ChecksumError(f"{path} doesn't exist")
    return parse(path.read_text(encoding="utf-8"), path)


def parse(text, source):
    entries = {}
    for number, line in enumerate(text.splitlines(), start=1):
        if not line.strip():
            continue
        match = LINE.match(line)
        if not match:
            raise ChecksumError(f"{source}:{number}: not a sha256sum line")
        name = match.group(3)
        if name in entries:
            raise ChecksumError(f"{source}:{number}: {name} is listed twice")
        entries[name] = line
    return entries


if __name__ == "__main__":
    sys.exit(main(sys.argv))
