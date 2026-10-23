#!/usr/bin/env python3
"""Writes the desktop updater manifest (latest.json) of a release (docs/updates.md §3.1).

  scripts/updater-manifest.py --version 1.0.1 --tag v1.0.1 --repo owner/name --dir dist \
      [--pub-date 2026-11-02T10:00:00Z] [--require-all] > dist/latest.json

Every update artifact in --dir needs its minisign signature next to it (`<file>.sig`, written by
`cargo tauri signer sign`). Keys are always `{os}-{arch}-{installer}`: the updater falls back to a
bare `{os}-{arch}` key, which would let a deb or rpm install "update" to an AppImage. With
--require-all every platform the release builds must be present.
"""
import argparse
import datetime
import json
import pathlib
import re
import sys
import urllib.parse

# Release asset name pattern -> updater targets it serves. The universal macOS app serves both
# architectures.
TARGETS = [
    (re.compile(r"_x64-setup\.exe$"), ["windows-x86_64-nsis"]),
    (re.compile(r"_arm64-setup\.exe$"), ["windows-aarch64-nsis"]),
    (re.compile(r"_x64_[A-Za-z-]+\.msi$"), ["windows-x86_64-msi"]),
    (re.compile(r"_arm64_[A-Za-z-]+\.msi$"), ["windows-aarch64-msi"]),
    (re.compile(r"_universal\.app\.tar\.gz$"), ["darwin-x86_64-app", "darwin-aarch64-app"]),
    (re.compile(r"_amd64\.AppImage$"), ["linux-x86_64-appimage"]),
    (re.compile(r"_aarch64\.AppImage$"), ["linux-aarch64-appimage"]),
]
ALL_TARGETS = sorted(target for _, targets in TARGETS for target in targets)
VERSION = re.compile(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$")


class ManifestError(Exception):
    pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--dir", required=True)
    parser.add_argument("--pub-date")
    parser.add_argument("--require-all", action="store_true")
    args = parser.parse_args()
    pub_date = args.pub_date or datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    try:
        manifest = build_manifest(
            pathlib.Path(args.dir), args.version, args.tag, args.repo, pub_date, args.require_all
        )
    except ManifestError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    json.dump(manifest, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


def build_manifest(
    directory: pathlib.Path, version: str, tag: str, repo: str, pub_date: str, require_all: bool
) -> dict:
    if not VERSION.match(version):
        raise ManifestError(f"{version!r} is not a semantic version")
    if not re.match(r"^[\w.-]+/[\w.-]+$", repo):
        raise ManifestError(f"{repo!r} is not owner/name")
    platforms: dict[str, dict[str, str]] = {}
    for signature_file in sorted(directory.glob("*.sig")):
        artifact = signature_file.with_suffix("")
        if not artifact.is_file():
            raise ManifestError(f"{signature_file.name} has no artifact next to it")
        targets = targets_of(artifact.name)
        if not targets:
            raise ManifestError(f"{artifact.name} is signed but no updater target uses it")
        signature = signature_file.read_text().strip()
        if not signature:
            raise ManifestError(f"{signature_file.name} is empty")
        url = f"https://github.com/{repo}/releases/download/{tag}/{urllib.parse.quote(artifact.name)}"
        for target in targets:
            if target in platforms:
                raise ManifestError(f"two artifacts for {target}: {platforms[target]['url']} and {url}")
            platforms[target] = {"url": url, "signature": signature}
    if not platforms:
        raise ManifestError(f"no signed update artifacts in {directory}")
    missing = [target for target in ALL_TARGETS if target not in platforms]
    if require_all and missing:
        raise ManifestError(f"missing update artifacts for {', '.join(missing)}")
    return {
        "version": version,
        "notes": f"https://github.com/{repo}/releases/tag/{tag}",
        "pub_date": pub_date,
        "platforms": platforms,
    }


def targets_of(name: str) -> list[str]:
    for pattern, targets in TARGETS:
        if pattern.search(name):
            return targets
    return []


if __name__ == "__main__":
    sys.exit(main())
