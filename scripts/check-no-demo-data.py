#!/usr/bin/env python3
"""Fails if a build output contains the UI's demo data (crates/teamcraft-ui/src/mock.rs).

Demo data must never reach a user: the mock backend only exists with the development-only `demo`
feature of teamcraft-ui. Release workflows run this on the web UI bundle (the trunk output that the
desktop apps embed and the server serves) before anything is packaged or published.

    scripts/check-no-demo-data.py crates/teamcraft-ui/dist [more paths or .tar.gz archives ...]

Directories are searched recursively; `.tar.gz`/`.tgz` archives are searched member by member.
"""
import argparse
import pathlib
import sys
import tarfile

# Names that only the demo data uses (people and spaces of the mock workspace). Rust keeps string
# literals verbatim (UTF-8) in the wasm data section, so a bundle with the mock compiled in has them.
DEMO_FIXTURES = [
    "Riley Guest",
    "Kim Novak",
    "Taylor Brooks",
    "Devon Park",
    "Casey Diaz",
    "Project A",
    "Project B",
]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="+", type=pathlib.Path, help="files, directories or .tar.gz archives")
    args = parser.parse_args(argv)
    findings = []
    scanned = 0
    for path in args.paths:
        if not path.exists():
            print(f"error: {path} does not exist", file=sys.stderr)
            return 2
        for name, data in read_all(path):
            scanned += 1
            findings.extend((name, fixture) for fixture in fixtures_in(data))
    if scanned == 0:
        print("error: no files to check", file=sys.stderr)
        return 2
    for name, fixture in findings:
        print(f"::error file={name}::demo data in a release build: {fixture!r}")
    if findings:
        print(f"{len(findings)} demo fixture(s) found; build without the teamcraft-ui `demo` feature", file=sys.stderr)
        return 1
    print(f"no demo data in {scanned} file(s)")
    return 0


def fixtures_in(data):
    """The demo fixtures that occur in `data` (bytes)."""
    return [fixture for fixture in DEMO_FIXTURES if fixture.encode() in data]


def read_all(path):
    """(name, bytes) of every file under `path`, archives opened."""
    if path.is_dir():
        for child in sorted(path.rglob("*")):
            if child.is_file():
                yield from read_file(child)
    else:
        yield from read_file(path)


def read_file(path):
    if path.name.endswith((".tar.gz", ".tgz")):
        with tarfile.open(path, "r:gz") as archive:
            for member in archive:
                if member.isfile():
                    extracted = archive.extractfile(member)
                    if extracted is not None:
                        yield f"{path}:{member.name}", extracted.read()
    else:
        yield str(path), path.read_bytes()


if __name__ == "__main__":
    sys.exit(main())
