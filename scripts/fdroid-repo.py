#!/usr/bin/env python3
"""The F-Droid repository of the package site (docs/updates.md §8): the libre Android app.

    fdroid-repo.py fetch --work <work> --repo <owner/name> [--keep N]
        The libre release APKs (teamcraft-android-X.Y.Z-libre-release.apk) and the release API
        objects of the newest N published stable releases that have one, into
        <work>/fdroid/<tag>/ (needs gh). Releases without a signed libre APK are skipped.
    fdroid-repo.py build --work <work> --site <site> --site-url <url> --repo <owner/name>
        Generates the signed repository with fdroidserver (`fdroid update`) from what fetch
        downloaded and puts it into <site>/fdroid/ in one rename: repo/ (the APKs, index-v1.jar,
        index-v2.json, entry.jar, icons, store texts and screenshots), the repository's QR code
        (qr.svg) and its fingerprint (fingerprint.txt, read by package-site-page.py).

Environment for build (the repository key, a Java keystore; docs/updates.md §8.3):
    FDROID_KEYSTORE           path of the keystore file
    FDROID_KEYSTORE_PASSWORD  its password
    FDROID_KEY_ALIAS          alias of the key in it
    FDROID_KEY_PASSWORD       the key's password (often the same as the keystore's)

Like the rest of the site the repository has no state of its own: every run rebuilds it from the
releases. The release APKs are used as they are, so the repository serves the very files of the
GitHub release, signed with the app key (apps/android/README.md, "Signing"), and installs from
either update each other.
"""
import argparse
import base64
import datetime
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys

APP_ID = "de.zeitgleich.teamcraft.libre"
APK_PATTERN = re.compile(r"^teamcraft-android-(?P<version>\d+\.\d+\.\d+)-libre-release\.apk$")
TAG_PATTERN = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
DEFAULT_KEEP = 3
SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
ROOT_DIR = SCRIPT_DIR.parent
STORE_DIR = ROOT_DIR / "apps" / "android" / "store"
REPO_ICON = ROOT_DIR / "apps" / "android" / "store" / "en-US" / "images" / "icon.png"
# The store texts describe both flavors; the libre app has UnifiedPush only.
LIBRE_TEXT_FIXES = {
    "en-US": [("Push through Firebase or UnifiedPush", "Push through UnifiedPush, without Google services")],
    "de-DE": [("Push über Firebase oder UnifiedPush", "Push über UnifiedPush, ohne Google-Dienste")],
}
# F-Droid shows at most 500 characters of a version's changelog.
MAX_CHANGELOG_CHARS = 500
KEY_ENV = ("FDROID_KEYSTORE", "FDROID_KEYSTORE_PASSWORD", "FDROID_KEY_ALIAS", "FDROID_KEY_PASSWORD")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    fetch_parser = commands.add_parser("fetch")
    fetch_parser.add_argument("--work", type=pathlib.Path, required=True)
    fetch_parser.add_argument("--repo", required=True)
    fetch_parser.add_argument("--keep", type=int, default=DEFAULT_KEEP)
    build_parser = commands.add_parser("build")
    build_parser.add_argument("--work", type=pathlib.Path, required=True)
    build_parser.add_argument("--site", type=pathlib.Path, required=True)
    build_parser.add_argument("--site-url", required=True)
    build_parser.add_argument("--repo", required=True)
    args = parser.parse_args()
    if args.command == "fetch":
        if args.keep < 1:
            fail("--keep must be at least 1")
        fetch(args.work.resolve(), args.repo, args.keep)
    else:
        build(args.work.resolve(), args.site.resolve(), args.site_url.rstrip("/"), args.repo)


def fetch(work, repo, keep):
    target = work / "fdroid"
    shutil.rmtree(target, ignore_errors=True)
    target.mkdir(parents=True)
    releases = json.loads(gh("api", f"repos/{repo}/releases?per_page=100"))
    candidates = sorted(
        (r for r in releases if not r["draft"] and not r["prerelease"] and TAG_PATTERN.match(r["tag_name"])),
        key=lambda r: tuple(int(p) for p in TAG_PATTERN.match(r["tag_name"]).groups()),
        reverse=True,
    )
    fetched = 0
    for release in candidates:
        assets = [a for a in release["assets"] if APK_PATTERN.match(a["name"])]
        if not assets:
            print(f"{release['tag_name']}: no signed libre APK, skipped")
            continue
        directory = target / release["tag_name"]
        directory.mkdir()
        (directory / "release.json").write_text(json.dumps(release), encoding="utf-8")
        gh("release", "download", release["tag_name"], "--repo", repo, "--dir", str(directory),
              "--pattern", assets[0]["name"], "--clobber")
        print(f"{release['tag_name']}: {assets[0]['name']}")
        fetched += 1
        if fetched == keep:
            break
    if not fetched:
        fail("no published stable release has a signed libre APK")


def build(work, site, site_url, repo):
    missing = [name for name in KEY_ENV if not os.environ.get(name)]
    if missing:
        fail(f"the repository key is not configured ({', '.join(missing)})")
    release_dirs = sorted((p.parent for p in (work / "fdroid").glob("v*/release.json")), key=tag_version_code)
    if not release_dirs:
        fail(f"nothing fetched in {work / 'fdroid'}")
    build_dir = work / "fdroid-build"
    shutil.rmtree(build_dir, ignore_errors=True)
    (build_dir / "repo").mkdir(parents=True)
    write_config(build_dir, site_url, repo)
    write_store_texts(build_dir)
    builds = [add_release(build_dir, directory) for directory in release_dirs]
    write_metadata(build_dir, site_url, repo, builds)
    # --use-date-from-apk: the "added" date of each version is the APK's mtime, set to the release's
    # publication in add_release; without it every rebuild would date all versions today.
    subprocess.run(["fdroid", "update", "--use-date-from-apk"], cwd=build_dir, check=True)
    fingerprint = key_fingerprint()
    check_repository(build_dir / "repo", len(builds), fingerprint)
    publish(build_dir, site, site_url, fingerprint)


def write_config(build_dir, site_url, repo):
    """config.yml for fdroidserver; the passwords stay in the environment ({env: NAME})."""
    shutil.copy(REPO_ICON, build_dir / "repo-icon.png")
    config = {
        "repo_url": f"{site_url}/fdroid/repo",
        "repo_name": "TeamCraft",
        "repo_icon": "repo-icon.png",
        "repo_description": (
            "The TeamCraft Android app without Google services (push through UnifiedPush), "
            f"straight from the releases of https://github.com/{repo}. Self-hosted, end-to-end "
            "encrypted team chat, calls and meetings."),
        # fetch keeps the newest versions only; no archive repository.
        "archive_older": 0,
        "keystore": os.environ["FDROID_KEYSTORE"],
        "repo_keyalias": os.environ["FDROID_KEY_ALIAS"],
        "keystorepass": {"env": "FDROID_KEYSTORE_PASSWORD"},
        "keypass": {"env": "FDROID_KEY_PASSWORD"},
    }
    path = build_dir / "config.yml"
    # JSON is YAML; fdroidserver refuses a config file others can read.
    path.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")
    path.chmod(0o600)


def write_store_texts(build_dir):
    """The store listing (apps/android/store, the same as Google Play's) in fastlane layout."""
    for locale_dir in sorted(p for p in STORE_DIR.iterdir() if p.is_dir()):
        target = build_dir / "metadata" / APP_ID / locale_dir.name
        shutil.copytree(locale_dir, target, ignore=shutil.ignore_patterns("*.svg"))
        description = target / "full_description.txt"
        text = description.read_text(encoding="utf-8")
        for old, new in LIBRE_TEXT_FIXES.get(locale_dir.name, []):
            if old not in text:
                fail(f"{locale_dir.name}/full_description.txt no longer says {old!r}; update LIBRE_TEXT_FIXES")
            text = text.replace(old, new)
        description.write_text(text, encoding="utf-8")


def add_release(build_dir, directory):
    """The APK into repo/, dated by the release, and its changelog from the release highlights.
    Returns the version's Builds entry."""
    release = json.loads((directory / "release.json").read_text(encoding="utf-8"))
    apks = [p for p in directory.iterdir() if APK_PATTERN.match(p.name)]
    if len(apks) != 1:
        fail(f"{directory}: expected one libre APK, found {len(apks)}")
    apk = build_dir / "repo" / apks[0].name
    shutil.copy(apks[0], apk)
    published = datetime.datetime.fromisoformat(release["published_at"].replace("Z", "+00:00")).timestamp()
    os.utime(apk, (published, published))
    version = APK_PATTERN.match(apk.name)["version"]
    changelogs = build_dir / "metadata" / APP_ID / "en-US" / "changelogs"
    changelogs.mkdir(parents=True, exist_ok=True)
    changelog = changelog_text(release.get("body") or "", release["html_url"])
    (changelogs / f"{version_code(version)}.txt").write_text(changelog + "\n", encoding="utf-8")
    return {"versionName": version, "versionCode": version_code(version), "commit": release["tag_name"]}


def write_metadata(build_dir, site_url, repo, builds):
    """The app's metadata file. fdroidserver attaches a changelog (changelogs/<versionCode>.txt) to a
    version only when it has a Builds entry; the APKs are the release's, nothing is built here."""
    repo_url = f"https://github.com/{repo}"
    metadata = {
        "Categories": ["Internet"],
        "License": "AGPL-3.0-or-later",
        "WebSite": site_url,
        "SourceCode": repo_url,
        "IssueTracker": f"{repo_url}/issues",
        "Changelog": f"{repo_url}/releases",
        "AutoName": "TeamCraft",
        "Builds": builds,
        "CurrentVersion": builds[-1]["versionName"],
        "CurrentVersionCode": builds[-1]["versionCode"],
    }
    path = build_dir / "metadata" / f"{APP_ID}.yml"
    path.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")


def changelog_text(body, release_url):
    """The bullets of the release's "Highlights" section (scripts/nightly-notes.py) as plain text."""
    match = re.search(r"^## Highlights\s*\n(.*?)(?=^## |\Z)", body, flags=re.MULTILINE | re.DOTALL)
    lines = []
    for line in (match.group(1) if match else "").splitlines():
        line = line.strip()
        if not line.startswith(("- ", "* ")):
            continue
        line = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", line[2:])
        lines.append("• " + line.replace("`", "").replace("**", ""))
    footer = f"Release notes: {release_url}"
    text = "\n".join(lines)
    budget = MAX_CHANGELOG_CHARS - len(footer) - 2
    if len(text) > budget:
        text = text[:budget - 1].rsplit("\n", 1)[0] + "\n…"
    return f"{text}\n\n{footer}" if text else footer


def tag_version_code(directory):
    return version_code(directory.name.removeprefix("v"))


def version_code(version):
    """docs/release.md, "Build numbers": the versionCode of a final release."""
    major, minor, patch = (int(p) for p in version.split("."))
    return major * 1_000_000 + minor * 10_000 + patch * 100 + 99


def check_repository(repo_dir, version_count, fingerprint):
    """The indexes are signed with the repository key, and list every version, all signed with one
    key, each with its changelog."""
    for name in ("index-v1.jar", "index-v2.json", "entry.jar", "index-v1.json"):
        if not (repo_dir / name).is_file():
            fail(f"fdroid update wrote no {name}")
    for name in ("index-v1.jar", "entry.jar"):
        if jar_signer(repo_dir / name) != fingerprint:
            fail(f"{name} is not signed with the repository key {fingerprint}")
    index = json.loads((repo_dir / "index-v2.json").read_text(encoding="utf-8"))
    versions = index.get("packages", {}).get(APP_ID, {}).get("versions", {})
    if len(versions) != version_count:
        fail(f"the index lists {len(versions)} versions of {APP_ID} instead of {version_count}")
    signers = {tuple(v["manifest"]["signer"]["sha256"]) for v in versions.values()}
    if len(signers) != 1:
        fail(f"the APKs are signed with {len(signers)} different keys; updates would not install")
    for version in versions.values():
        if not version.get("whatsNew"):
            fail(f"versionCode {version['manifest']['versionCode']} has no changelog (docs/release.md formula?)")


def publish(build_dir, site, site_url, fingerprint):
    """repo/, qr.svg and fingerprint.txt into <site>/fdroid/ in one rename."""
    staging = site / ".fdroid-staging"
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)
    # status/ is fdroidserver's run log (paths, host details); clients don't read it.
    shutil.copytree(build_dir / "repo", staging / "repo", ignore=shutil.ignore_patterns("status"))
    (staging / "fingerprint.txt").write_text(fingerprint + "\n", encoding="utf-8")
    repo_link = f"{site_url}/fdroid/repo?fingerprint={fingerprint}"
    subprocess.run(["qrencode", "--type=SVG", "--margin=2", "--output", str(staging / "qr.svg"), repo_link], check=True)
    target = site / "fdroid"
    shutil.rmtree(target, ignore_errors=True)
    staging.rename(target)
    print(f"F-Droid repository: {repo_link}")


def key_fingerprint():
    """The repository fingerprint: SHA-256 of the signing certificate (DER), upper-case hex."""
    certificate = subprocess.run(
        ["keytool", "-exportcert", "-keystore", os.environ["FDROID_KEYSTORE"],
          "-alias", os.environ["FDROID_KEY_ALIAS"], "-storepass:env", "FDROID_KEYSTORE_PASSWORD"],
        check=True, capture_output=True).stdout
    return hashlib.sha256(certificate).hexdigest().upper()


def jar_signer(path):
    """SHA-256 of a signed JAR's certificate, like key_fingerprint. (jarsigner -verify can't check
    index-v1.jar: it keeps the SHA-1 digest old clients need.)"""
    certificate = subprocess.run(["keytool", "-printcert", "-rfc", "-jarfile", str(path)],
                                 check=True, capture_output=True, text=True).stdout
    match = re.search(r"-----BEGIN CERTIFICATE-----(.*?)-----END CERTIFICATE-----", certificate, re.DOTALL)
    if not match:
        fail(f"{path.name} has no signature")
    return hashlib.sha256(base64.b64decode("".join(match.group(1).split()))).hexdigest().upper()


def gh(*args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def fail(message):
    if os.environ.get("GITHUB_ACTIONS"):
        print(f"::error::{message}")
    sys.exit(f"error: {message}")


if __name__ == "__main__":
    main()
