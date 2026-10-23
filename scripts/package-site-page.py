#!/usr/bin/env python3
"""The landing page of the package site (docs/updates.md §5) and its link check.

    package-site-page.py render --work <work> --site <site> --site-url <url> --repo <owner/name>
            Writes <site>/index.html and its static files from what the site build published: the
            releases in <work>/{stable,prerelease}/release.json (GitHub release API objects) and the
            repositories and manifests that exist in <site>. Downloads link the release assets; only
            assets that exist are linked.
    package-site-page.py check --site <site> --site-url <url>
            Fails when a page of the site links a file of the site that doesn't exist (href, src, and
            site URLs in the setup snippets).

Environment for render (all optional):
    GPG_KEY_ID      fingerprint of the repository key, shown for verification
    PLAY_STORE_URL  Google Play listing; shown on the Android card when set
    APP_STORE_URL   App Store listing; offered to iPhone and iPad visitors when set
"""
import argparse
import gzip
import html
import json
import os
import pathlib
import re
import shutil
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from html.parser import HTMLParser
from urllib.parse import urldefrag, urlsplit

CHANNELS = ("stable", "prerelease")
STATIC_DIR = pathlib.Path(__file__).resolve().parent / "site"
ICON_DIR = pathlib.Path(__file__).resolve().parent.parent / "apps" / "desktop" / "icons"
# Repository file -> published name.
ICONS = {"128x128.png": "logo.png", "32x32.png": "favicon.png", "128x128@2x.png": "apple-touch-icon.png"}
DEPLOY_REPO = "Zeitgleich/teamcraft-deploy"
DEFAULT_PACKAGE = "team-craft"
CSP = (
    "default-src 'none'; img-src 'self'; style-src 'self'; script-src 'self'; "
    "base-uri 'none'; form-action 'none'"
)
OS_TITLES = {"windows": "Windows", "macos": "macOS", "linux": "Linux", "android": "Android"}
ARCH_TITLES = {"x64": "x64", "arm64": "ARM64", "universal": "Apple silicon + Intel"}


@dataclass(frozen=True)
class Download:
    """One installable release asset."""

    os: str
    arch: str
    kind: str
    label: str
    name: str
    url: str
    size_bytes: int
    # The asset the page offers first for its OS and architecture.
    is_primary: bool = False
    note: str = ""


@dataclass
class Channel:
    name: str
    tag: str
    title: str
    published: str
    notes_url: str
    downloads: list = field(default_factory=list)
    maybe_checksums_url: str = ""

    @property
    def is_prerelease(self):
        return self.name == "prerelease"


# (regex on the asset name, os, kind, label, is_primary, note); the arch is the regex group "arch".
RULES = [
    (r"_(?P<arch>x64|arm64)-setup\.exe$", "windows", "exe", "Installer (.exe)", True, ""),
    (r"_(?P<arch>x64|arm64)_[A-Za-z-]+\.msi$", "windows", "msi", "MSI package", False,
      "For managed installs (Group Policy, Intune)"),
    (r"_(?P<arch>universal)\.dmg$", "macos", "dmg", "Disk image (.dmg)", True, ""),
    (r"_(?P<arch>amd64|aarch64)\.AppImage$", "linux", "appimage", "AppImage", True,
      "Runs on most distributions, updates itself"),
    (r"_(?P<arch>amd64|arm64)\.deb$", "linux", "deb", "Debian / Ubuntu (.deb)", False, ""),
    (r"\.(?P<arch>x86_64|aarch64)\.rpm$", "linux", "rpm", "Fedora / openSUSE (.rpm)", False, ""),
    (r"-(?P<arch>x86_64|aarch64)\.flatpak$", "linux", "flatpak", "Flatpak bundle", False, ""),
    (r"-full-release\.apk$", "android", "apk-full", "APK", True,
      "Push notifications through Google Play services"),
    (r"-libre-release\.apk$", "android", "apk-libre", "APK (libre)", False,
      "Without Google services, push through UnifiedPush"),
]
# docs/release.md: debug-signed APKs use a throwaway key; offered only on the prerelease channel
# when the release has no signed release APK.
DEBUG_RULES = [
    (r"-full-debug-signed\.apk$", "android", "apk-full", "APK (test build)", True,
      "Push through Google Play services. Signed with a test key: can't update a store install"),
    (r"-libre-debug-signed\.apk$", "android", "apk-libre", "APK (libre, test build)", False,
      "Without Google services. Signed with a test key: can't update a store install"),
]
ARCH_ALIASES = {"amd64": "x64", "x86_64": "x64", "aarch64": "arm64"}
OS_ORDER = list(OS_TITLES)
KIND_ORDER = ["exe", "msi", "dmg", "appimage", "deb", "rpm", "flatpak", "apk-full", "apk-libre"]
ARCH_ORDER = ["universal", "x64", "arm64", ""]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    render_parser = commands.add_parser("render")
    render_parser.add_argument("--work", type=pathlib.Path, required=True)
    render_parser.add_argument("--site", type=pathlib.Path, required=True)
    render_parser.add_argument("--site-url", required=True)
    render_parser.add_argument("--repo", required=True)
    check_parser = commands.add_parser("check")
    check_parser.add_argument("--site", type=pathlib.Path, required=True)
    check_parser.add_argument("--site-url", required=True)
    args = parser.parse_args()
    site_url = args.site_url.rstrip("/")
    if args.command == "render":
        channels = load_channels(args.work)
        if not channels:
            sys.exit(f"error: no release in {args.work}")
        copy_static(args.site)
        page = render_page(channels, args.site, site_url, args.repo, os.environ)
        (args.site / "index.html").write_text(page, encoding="utf-8")
    else:
        broken = check_links(args.site, site_url)
        for page, target in broken:
            print(f"error: {page} links {target}, which isn't on the site", file=sys.stderr)
        if broken:
            sys.exit(1)
        print(f"links ok in {args.site}")


def load_channels(work):
    """The channels that have a release, stable first."""
    channels = {}
    for name in CHANNELS:
        path = work / name / "release.json"
        if path.is_file():
            channels[name] = channel_from_release(name, json.loads(path.read_text(encoding="utf-8")))
    return channels


def channel_from_release(name, release):
    assets = release.get("assets", [])
    checksums = [a["browser_download_url"] for a in assets if a["name"] == "SHA256SUMS"]
    return Channel(
        name=name,
        tag=release["tag_name"],
        title=release.get("name") or release["tag_name"],
        published=(release.get("published_at") or "")[:10],
        notes_url=release["html_url"],
        downloads=classify_assets(assets, allow_debug=name == "prerelease"),
        maybe_checksums_url=checksums[0] if checksums else "",
    )


def classify_assets(assets, allow_debug):
    """The installable assets, in display order. Signatures, SBOMs and unsigned APKs are skipped."""
    downloads = match_rules(assets, RULES)
    has_release_apk = any(d.os == "android" for d in downloads)
    if allow_debug and not has_release_apk:
        downloads += match_rules(assets, DEBUG_RULES)
    return sorted(downloads, key=lambda d: (
        OS_ORDER.index(d.os), KIND_ORDER.index(d.kind), ARCH_ORDER.index(d.arch)))


def match_rules(assets, rules):
    downloads = []
    for asset in assets:
        for pattern, os_name, kind, label, is_primary, note in rules:
            match = re.search(pattern, asset["name"])
            if not match:
                continue
            arch = match.groupdict().get("arch") or ""
            downloads.append(Download(
                os=os_name, arch=ARCH_ALIASES.get(arch, arch), kind=kind, label=label, name=asset["name"],
                url=asset["browser_download_url"], size_bytes=int(asset.get("size", 0)),
                is_primary=is_primary, note=note,
            ))
            break
    return downloads


def copy_static(site):
    site.mkdir(parents=True, exist_ok=True)
    for path in STATIC_DIR.iterdir():
        shutil.copy(path, site / path.name)
    for source, target in ICONS.items():
        shutil.copy(ICON_DIR / source, site / target)


def render_page(channels, site, site_url, repo, env):
    maybe_stable = channels.get("stable")
    maybe_prerelease = channels.get("prerelease")
    # The channel the page recommends: stable once there is one.
    main_channel = maybe_stable or maybe_prerelease
    repo_url = f"https://github.com/{repo}"
    body = "\n".join([
        render_header(repo_url),
        "<main>",
        render_hero(main_channel, env.get("APP_STORE_URL", "")),
        render_downloads(maybe_stable, maybe_prerelease, env),
        render_linux_repositories(channels, site, site_url),
        render_verify(channels, site, repo, env),
        render_self_hosting(repo_url),
        "</main>",
        render_footer(channels, site, repo_url),
    ])
    return PAGE.format(csp=CSP, body=body)


def render_header(repo_url):
    return f"""<header class="top">
    <div class="wrap top-inner">
        <a class="brand" href="#top" aria-label="TeamCraft home">
            <img src="logo.png" alt="" width="32" height="32"><span>TeamCraft</span>
        </a>
        <nav aria-label="Sections">
            <a href="#download">Download</a>
            <a href="#linux">Linux repositories</a>
            <a href="#verify">Verify</a>
            <a href="#self-hosting">Self-hosting</a>
            <a href="{esc(repo_url)}">GitHub</a>
        </nav>
    </div>
</header>"""


def render_hero(channel, maybe_app_store_url):
    badge = prerelease_badge() if channel.is_prerelease else ""
    return f"""<section class="hero" id="top">
    <div class="wrap hero-inner">
        <div class="hero-text">
            <h1>Team chat that keeps your conversations yours.</h1>
            <p class="lead">TeamCraft is a self-hosted team chat with channels, threads, direct messages,
            calls and meetings. Everything is end-to-end encrypted on your devices, so the server you run it
            on never sees messages, files or calls. Apps for Windows, macOS, Linux, Android and the web.</p>
            <div class="cta" data-channel="{esc(channel.name)}" data-app-store="{esc(maybe_app_store_url)}">
                <a class="button primary" id="cta-button" href="#download">Download TeamCraft</a>
                <a class="button ghost" href="#download">All downloads</a>
            </div>
            <p class="cta-meta" id="cta-meta">{esc(channel.tag)} {badge}</p>
        </div>
        <ul class="features" aria-label="Highlights">
            <li><strong>End-to-end encrypted</strong><span>Messages, files, calls and search, with MLS
            and per-device keys.</span></li>
            <li><strong>Calls and meetings</strong><span>Screen sharing, guests with a lobby, and
            meeting invitations.</span></li>
            <li><strong>Runs on your server</strong><span>One container setup; the server is treated
            as untrusted.</span></li>
            <li><strong>Open source</strong><span>AGPL-3.0, reproducible releases with build
            provenance.</span></li>
        </ul>
    </div>
</section>"""


def render_downloads(maybe_stable, maybe_prerelease, env):
    parts = ['<section class="section" id="download">', '<div class="wrap">', "<h2>Download</h2>"]
    if maybe_stable:
        parts.append(render_channel(maybe_stable, env))
    else:
        parts.append("""<div class="notice">
    <strong>The first stable release is coming soon.</strong>
    <span>Until then you can try the newest release candidate below. It is feature complete but may
    still have rough edges.</span>
</div>""")
    if maybe_prerelease and (not maybe_stable or maybe_prerelease.tag != maybe_stable.tag):
        if maybe_stable:
            parts.append(f"""<details class="channel-more">
    <summary>Pre-release {esc(maybe_prerelease.tag)} {prerelease_badge()}</summary>
    {render_channel(maybe_prerelease, env)}
</details>""")
        else:
            parts.append(render_channel(maybe_prerelease, env))
    parts += ["</div>", "</section>"]
    return "\n".join(parts)


def render_channel(channel, env):
    badge = prerelease_badge() if channel.is_prerelease else '<span class="badge stable">Stable</span>'
    date = f' · <time datetime="{esc(channel.published)}">{esc(channel.published)}</time>' if channel.published else ""
    cards = "\n".join(render_platform(channel, os_name, env) for os_name in OS_TITLES)
    return f"""<div class="channel" data-channel-block="{esc(channel.name)}">
    <p class="channel-head">{badge} <strong>{esc(channel.title)}</strong>{date} ·
    <a href="{esc(channel.notes_url)}">Release notes</a></p>
    <div class="platforms">
{cards}
    </div>
</div>"""


def render_platform(channel, os_name, env):
    downloads = [d for d in channel.downloads if d.os == os_name]
    # One row per package kind, with a link per architecture.
    kinds = list(dict.fromkeys(d.kind for d in downloads))
    rows = "\n".join(
        render_download_row([d for d in downloads if d.kind == kind], channel) for kind in kinds)
    extra = ""
    if os_name == "android" and env.get("PLAY_STORE_URL"):
        extra += f'<a class="button ghost store" href="{esc(env["PLAY_STORE_URL"])}">Get it on Google Play</a>'
    if os_name == "linux" and downloads:
        extra += '<p class="hint">Or add the <a href="#linux">package repository</a> for updates through apt, dnf or zypper.</p>'
    if os_name == "macos" and downloads:
        extra += '<p class="hint">macOS 14 or newer.</p>'
    if not rows and not extra:
        rows = '<li class="hint">Not part of this release.</li>'
    return f"""<article class="platform" data-os="{os_name}">
    <h3>{ICONS_SVG[os_name]}{OS_TITLES[os_name]}<span class="detected" hidden>Your system</span></h3>
    <ul class="downloads">
{rows}
    </ul>
    {extra}
</article>"""


def render_download_row(downloads, channel):
    first = downloads[0]
    note = f'<span class="note">{esc(first.note)}</span>' if first.note else ""
    links = "".join(render_download(d, channel) for d in downloads)
    return (f'    <li class="download-row"><span class="label">{esc(first.label)}</span>{note}'
            f'<span class="archs">{links}</span></li>')


def render_download(download, channel):
    arch = ARCH_TITLES.get(download.arch, download.arch)
    size = format_size(download.size_bytes)
    meta = " · ".join(x for x in (arch, size) if x)
    arch = arch or "Download"
    primary = " data-primary" if download.is_primary else ""
    return (
        f'<a class="download" href="{esc(download.url)}" data-os="{download.os}" '
        f'data-arch="{esc(download.arch)}" data-kind="{download.kind}" data-label="{esc(download.label)}" '
        f'data-meta="{esc(meta)} · {esc(channel.tag)}"{primary} '
        f'aria-label="{esc(download.label)}, {esc(meta)}">'
        f'<span class="arch">{esc(arch)}</span><span class="size">{esc(size)}</span></a>'
    )


def render_linux_repositories(channels, site, site_url):
    blocks = []
    for channel in channels.values():
        block = render_repository_channel(channel, site, site_url)
        if not block:
            continue
        if channel.is_prerelease and "stable" in channels and blocks:
            blocks.append(f"""<details class="channel-more">
    <summary>Pre-release channel {prerelease_badge()}</summary>
    {block}
</details>""")
        else:
            blocks.append(block)
    if not blocks:
        return ""
    return f"""<section class="section alt" id="linux">
<div class="wrap">
    <h2>Linux package repositories</h2>
    <p class="section-lead">Install TeamCraft with your package manager and get new versions with your
    system updates. Packages and repository metadata are signed with the
    <a href="#verify">repository key</a>.</p>
    {"".join(blocks)}
</div>
</section>"""


def render_repository_channel(channel, site, site_url):
    has_apt = (site / "apt" / "dists" / channel.name / "InRelease").is_file()
    repo_name = "teamcraft" if channel.name == "stable" else f"teamcraft-{channel.name}"
    has_rpm = (site / "rpm" / channel.name / f"{repo_name}.repo").is_file()
    if not has_apt and not has_rpm:
        return ""
    parts = []
    if channel.is_prerelease:
        parts.append(f'<p class="channel-head">{prerelease_badge()} Release candidates and previews '
                                  f'({esc(channel.tag)} today). Stable releases arrive here too.</p>')
    parts.append('<div class="snippets">')
    if has_apt:
        package = apt_package_name(site, channel.name)
        apt = f"""sudo curl -fsSLo /usr/share/keyrings/teamcraft-archive-keyring.gpg \\
    {site_url}/teamcraft-archive-keyring.gpg
sudo tee /etc/apt/sources.list.d/teamcraft.sources >/dev/null <<'EOF'
Types: deb
URIs: {site_url}/apt
Suites: {channel.name}
Components: main
Signed-By: /usr/share/keyrings/teamcraft-archive-keyring.gpg
EOF
sudo apt update && sudo apt install {package}"""
        parts.append(render_snippet(f"apt-{channel.name}", "Debian, Ubuntu (APT)", apt, is_wide=True))
    if has_rpm:
        package = rpm_package_name(site, channel.name)
        repo_url = f"{site_url}/rpm/{channel.name}/{repo_name}.repo"
        dnf = f"""sudo curl -fsSLo /etc/yum.repos.d/{repo_name}.repo \\
    {repo_url}
sudo dnf install {package}"""
        zypper = f"""sudo zypper addrepo \\
    {repo_url}
sudo zypper install {package}"""
        parts.append(render_snippet(f"dnf-{channel.name}", "Fedora, RHEL (DNF)", dnf))
        parts.append(render_snippet(f"zypper-{channel.name}", "openSUSE (zypper)", zypper))
    parts.append("</div>")
    links = []
    if has_rpm:
        links.append(f'<a href="rpm/{channel.name}/{repo_name}.repo">{repo_name}.repo</a>')
    links.append('<a href="teamcraft-archive-keyring.asc">Repository key (armored)</a>')
    parts.append(f'<p class="hint">Files: {" · ".join(links)}</p>')
    return "\n".join(parts)


def render_snippet(snippet_id, title, text, is_wide=False):
    wide = " wide" if is_wide else ""
    return f"""<figure class="snippet{wide}">
    <figcaption><span>{esc(title)}</span>
    <button type="button" class="copy" data-copy="{esc(snippet_id)}" hidden>Copy</button></figcaption>
    <pre id="{esc(snippet_id)}"><code>{esc(text)}</code></pre>
</figure>"""


def render_verify(channels, site, repo, env):
    checksum_links = " · ".join(
        f'<a href="{esc(c.maybe_checksums_url)}">SHA256SUMS of {esc(c.tag)}</a>'
        for c in channels.values() if c.maybe_checksums_url)
    items = []
    if checksum_links:
        items.append(f"""<li><h3>Checksums</h3>
    <p>Every release lists the SHA-256 of its files: {checksum_links}. Download it next to the file and
    run:</p>
    {render_snippet("verify-sha", "Linux, macOS", "sha256sum --check --ignore-missing SHA256SUMS")}
    {render_snippet("verify-sha-win", "Windows (PowerShell)", "Get-FileHash .\\TeamCraft_*-setup.exe -Algorithm SHA256")}
</li>""")
    items.append(f"""<li><h3>Build provenance</h3>
    <p>Release files are built by GitHub Actions from the tagged source and carry a signed build
    provenance attestation. With the GitHub CLI:</p>
    {render_snippet("verify-attestation", "Any system", f"gh attestation verify <file> --repo {repo}")}
</li>""")
    fingerprint = re.sub(r"\s+", "", env.get("GPG_KEY_ID", "")).upper()
    if fingerprint and (site / "teamcraft-archive-keyring.asc").is_file():
        grouped = " ".join(fingerprint[i:i + 4] for i in range(0, len(fingerprint), 4))
        items.append(f"""<li><h3>Repository key</h3>
    <p>APT and RPM metadata and the rpm packages on this site are signed with the
    <a href="teamcraft-archive-keyring.asc">repository key</a>. Its fingerprint:</p>
    <p class="fingerprint"><code>{esc(grouped)}</code></p>
    {render_snippet("verify-key", "Check a downloaded key", "gpg --show-keys teamcraft-archive-keyring.asc")}
    <p class="hint">The rpms here are signed copies, so their checksums differ from the release's
    SHA256SUMS.</p>
</li>""")
    items.append("""<li><h3>Desktop updates</h3>
    <p>Installed desktop apps update themselves only with packages signed by the TeamCraft updater key;
    the app checks the signature before installing.</p>
</li>""")
    return f"""<section class="section" id="verify">
<div class="wrap">
    <h2>Verify your download</h2>
    <ul class="verify">
{"".join(items)}
    </ul>
</div>
</section>"""


def render_self_hosting(repo_url):
    return f"""<section class="section alt" id="self-hosting">
<div class="wrap self-hosting">
    <div>
        <h2>Run your own server</h2>
        <p class="section-lead">The apps connect to a TeamCraft server run by your organization. The
        deployment repository has a ready Docker Compose setup with the server, media relay, identity
        provider and database, plus upgrade and backup guides.</p>
    </div>
    <div class="self-hosting-links">
        <a class="button primary" href="https://github.com/{DEPLOY_REPO}">Deployment guide</a>
        <a class="button ghost" href="{esc(repo_url)}/blob/main/docs/operations.md">Operations docs</a>
    </div>
</div>
</section>"""


def render_footer(channels, site, repo_url):
    manifests = [
        f'<a href="updates/{name}/latest.json">{name}</a>'
        for name in channels if (site / "updates" / name / "latest.json").is_file()
    ]
    manifest_text = f" · Update manifests: {', '.join(manifests)}" if manifests else ""
    return f"""<footer class="bottom">
    <div class="wrap">
        <p>TeamCraft is free software under the
        <a href="{esc(repo_url)}/blob/main/LICENSE">GNU AGPL-3.0</a> ·
        <a href="{esc(repo_url)}">Source code</a> ·
        <a href="{esc(repo_url)}/releases">All releases</a> ·
        <a href="{esc(repo_url)}/blob/main/SECURITY.md">Security policy</a> ·
        <a href="{esc(repo_url)}/blob/main/docs/updates.md">Packaging docs</a>{manifest_text}</p>
    </div>
</footer>"""


def apt_package_name(site, channel):
    packages = site / "apt" / "dists" / channel / "main" / "binary-amd64" / "Packages"
    if packages.is_file():
        for line in packages.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.startswith("Package: "):
                return line.removeprefix("Package: ").strip()
    return DEFAULT_PACKAGE


def rpm_package_name(site, channel):
    """The package name from the repository's primary metadata."""
    repomd = site / "rpm" / channel / "repodata" / "repomd.xml"
    if not repomd.is_file():
        return DEFAULT_PACKAGE
    ns = {"repo": "http://linux.duke.edu/metadata/repo", "common": "http://linux.duke.edu/metadata/common"}
    for data in ET.parse(repomd).getroot().findall("repo:data", ns):
        location = data.find("repo:location", ns)
        if data.get("type") != "primary" or location is None or not location.get("href", "").endswith(".gz"):
            continue
        with gzip.open(site / "rpm" / channel / location.get("href")) as primary:
            name = ET.parse(primary).getroot().find("common:package/common:name", ns)
        if name is not None and name.text:
            return name.text
    return DEFAULT_PACKAGE


def prerelease_badge():
    return '<span class="badge pre">Pre-release</span>'


def format_size(size_bytes):
    if size_bytes <= 0:
        return ""
    return f"{size_bytes / 1_000_000:.0f} MB" if size_bytes >= 1_000_000 else f"{size_bytes / 1000:.0f} kB"


def esc(text):
    return html.escape(str(text), quote=True)


def check_links(site, site_url):
    """(page, target) for every link to a file of the site that doesn't exist."""
    broken = []
    for page in sorted(site.rglob("*.html")):
        collector = LinkCollector()
        collector.feed(page.read_text(encoding="utf-8"))
        targets = [(t, False) for t in collector.links]
        # Site URLs in snippets (apt URIs, repo files, keys); a base URL like .../apt may be a directory.
        targets += [(t, True) for t in re.findall(re.escape(site_url) + r"/[^\s'\"<>\\]*", collector.text)]
        for target, may_be_directory in targets:
            maybe_path = site_path(page, target, site, site_url)
            if maybe_path is None:
                continue
            exists = maybe_path.is_file() or (maybe_path.is_dir() and (
                may_be_directory or (maybe_path / "index.html").is_file()))
            if not exists:
                broken.append((str(page.relative_to(site)), target))
    return broken


def site_path(page, target, site, site_url):
    """The file a link points to when it is on the site, else None."""
    target = urldefrag(html.unescape(target)).url
    if not target:
        return None
    if target.startswith(site_url + "/") or target == site_url:
        relative = target[len(site_url):].lstrip("/")
        return site / relative if relative else site / "index.html"
    parts = urlsplit(target)
    if parts.scheme or parts.netloc or target.startswith(("mailto:", "data:")):
        return None
    path = (site / parts.path.lstrip("/")) if parts.path.startswith("/") else (page.parent / parts.path)
    return path.resolve()


class LinkCollector(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []
        self.text_parts = []

    @property
    def text(self):
        return "".join(self.text_parts)

    def handle_starttag(self, tag, attrs):
        for name, value in attrs:
            if name in ("href", "src") and value:
                self.links.append(value)

    def handle_data(self, data):
        self.text_parts.append(data)


# Simple line icons, no vendor logos.
ICONS_SVG = {
    "windows": '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8M12 17v4"/></svg>',
    "macos": '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="4" y="5" width="16" height="11" rx="2"/><path d="M2 19h20"/></svg>',
    "linux": '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 9l3 3-3 3M12 15h5"/></svg>',
    "android": '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="2" width="12" height="20" rx="2.5"/><path d="M10.5 18.5h3"/></svg>',
}

PAGE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="{csp}">
<meta name="referrer" content="no-referrer">
<meta name="color-scheme" content="light dark">
<meta name="description" content="TeamCraft: self-hosted, end-to-end encrypted team chat, calls and meetings. Downloads for Windows, macOS, Linux and Android.">
<title>TeamCraft – encrypted team chat, calls and meetings</title>
<link rel="icon" href="favicon.png" type="image/png">
<link rel="apple-touch-icon" href="apple-touch-icon.png">
<link rel="stylesheet" href="style.css">
<script src="site.js" defer></script>
</head>
<body>
{body}
</body>
</html>
"""

if __name__ == "__main__":
    main()
