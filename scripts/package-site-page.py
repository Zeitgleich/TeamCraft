#!/usr/bin/env python3
"""The landing page of the package site (docs/updates.md §5) and its link check.

    package-site-page.py render --work <work> --site <site> --site-url <url> --repo <owner/name>
            Writes <site>/index.html and its static files from what the site build published: the
            releases in <work>/{stable,prerelease}/release.json (GitHub release API objects) and the
            repositories and manifests that exist in <site>. Downloads link the release assets; only
            assets that exist are linked.
    package-site-page.py check --site <site> --site-url <url>
            Fails when a page of the site links a file of the site that doesn't exist (href, src,
            url() in its style sheets, and site URLs in the setup snippets).

Environment for render (all optional):
    GPG_KEY_ID      fingerprint of the repository key, shown for verification
    PLAY_STORE_URL  Google Play listing; shown on the Android card when set
    APP_STORE_URL   App Store listing; offered to iPhone and iPad visitors when set

The static files are in scripts/site/ (published as they are) and the fixed illustrations of the page
in scripts/site-parts/ (inserted into index.html).
"""
import argparse
import datetime
import gzip
import html
import json
import os
import pathlib
import random
import re
import shutil
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from html.parser import HTMLParser
from urllib.parse import urldefrag, urlsplit

CHANNELS = ("stable", "prerelease")
SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
STATIC_DIR = SCRIPT_DIR / "site"
PARTS_DIR = SCRIPT_DIR / "site-parts"
ICON_DIR = SCRIPT_DIR.parent / "apps" / "desktop" / "icons"
# Repository file -> published name.
ICONS = {"128x128.png": "logo.png", "32x32.png": "favicon.png", "128x128@2x.png": "apple-touch-icon.png"}
DEPLOY_REPO = "Zeitgleich/teamcraft-deploy"
DEFAULT_PACKAGE = "team-craft"
CSP = (
    "default-src 'none'; img-src 'self'; style-src 'self'; script-src 'self'; font-src 'self'; "
    "base-uri 'none'; form-action 'none'"
)
OS_TITLES = {"windows": "Windows", "macos": "macOS", "linux": "Linux", "android": "Android"}
ARCH_TITLES = {"x64": "x64", "arm64": "ARM64"}
# Where an OS calls its architectures something else.
OS_ARCH_TITLES = {"macos": {"arm64": "Apple silicon", "x64": "Intel"}}
# What the main button of a platform card downloads.
KIND_BUTTONS = {"exe": "installer", "dmg": "disk image", "appimage": "AppImage", "apk-full": "APK"}
# Platforms whose main download is a choice between architectures the visitor has to know: a button
# each, with a hint, instead of one button and "Also for" links.
ARCH_CHOICES = {
    "macos": {
        "hints": {"arm64": "M1 and newer, most Macs since 2020", "x64": "Older Macs with an Intel processor"},
        "help": ("<strong>Which Mac do I have?</strong> Apple menu &rsaquo; About This Mac: "
                 "&ldquo;Chip: Apple M&hellip;&rdquo; means Apple silicon, "
                 "&ldquo;Processor: &hellip; Intel&rdquo; means Intel."),
    },
}
# Fixed requirements shown on a platform card, before what the assets say.
OS_REQUIREMENTS = {"macos": "macOS 14 or newer"}


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
    # Signed with the throwaway debug key (docs/release.md): can't replace a store install.
    is_test_build: bool = False


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
    (r"_(?P<arch>aarch64|x64)\.dmg$", "macos", "dmg", "Disk image (.dmg)", True, ""),
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
    (r"-full-debug-signed\.apk$", "android", "apk-full", "APK", True,
      "Push notifications through Google Play services"),
    (r"-libre-debug-signed\.apk$", "android", "apk-libre", "APK (libre)", False,
      "Without Google services, push through UnifiedPush"),
]
ARCH_ALIASES = {"amd64": "x64", "x86_64": "x64", "aarch64": "arm64"}
OS_ORDER = list(OS_TITLES)
KIND_ORDER = ["exe", "msi", "dmg", "appimage", "deb", "rpm", "flatpak", "apk-full", "apk-libre"]
ARCH_ORDER = ["x64", "arm64", ""]
# Where another architecture comes first: nearly every Mac sold since 2020 has Apple silicon.
OS_ARCH_ORDER = {"macos": ["arm64", "x64", ""]}
# English month names regardless of the build machine's locale.
MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]


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
        downloads += match_rules(assets, DEBUG_RULES, is_test_build=True)
    return sorted(downloads, key=lambda d: (
        OS_ORDER.index(d.os), KIND_ORDER.index(d.kind), OS_ARCH_ORDER.get(d.os, ARCH_ORDER).index(d.arch)))


def match_rules(assets, rules, is_test_build=False):
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
                is_primary=is_primary, note=note, is_test_build=is_test_build,
            ))
            break
    return downloads


def copy_static(site):
    site.mkdir(parents=True, exist_ok=True)
    shutil.copytree(STATIC_DIR, site, dirs_exist_ok=True)
    for source, target in ICONS.items():
        shutil.copy(ICON_DIR / source, site / target)


def render_page(channels, site, site_url, repo, env):
    maybe_stable = channels.get("stable")
    maybe_prerelease = channels.get("prerelease")
    # The channel the page recommends: stable once there is one.
    main_channel = maybe_stable or maybe_prerelease
    repo_url = f"https://github.com/{repo}"
    linux_repositories = render_linux_repositories(channels, main_channel, site, site_url)
    body = "\n".join([
        render_header(repo_url),
        '<main id="main">',
        render_hero(main_channel, env.get("APP_STORE_URL", "")),
        render_features(main_channel, env),
        render_downloads(maybe_stable, maybe_prerelease, env, has_linux_repositories=bool(linux_repositories)),
        render_channels(maybe_stable, maybe_prerelease),
        linux_repositories,
        render_verify(channels, site, repo, env),
        render_self_hosting(repo_url),
        "</main>",
        render_footer(channels, site, repo_url, has_linux_repositories=bool(linux_repositories)),
    ])
    return PAGE.format(csp=CSP, body=body)


def render_header(repo_url):
    return f"""<a class="skip-link" href="#main">Skip to content</a>
<header class="site-header">
  <div class="wrap header-inner">
    <a class="brand" href="#top">
      <img src="logo.png" alt="" width="28" height="28"><span>TeamCraft</span>
    </a>
    <nav class="header-nav" aria-label="Sections">
      <a href="#features">Features</a>
      <a href="#download">Download</a>
      <a href="#channels">Channels</a>
      <a href="#verify">Verify</a>
      <a href="#self-hosting">Self-hosting</a>
    </nav>
    <div class="header-actions">
      <a class="header-link" href="{esc(repo_url)}">GitHub</a>
      <a class="button button-primary button-small" href="#download">Download</a>
    </div>
  </div>
</header>"""


def render_hero(channel, maybe_app_store_url):
    channel_name = "Beta" if channel.is_prerelease else "Stable"
    date = f" · {esc(format_date(channel.published))}" if channel.published else ""
    return f"""<section class="hero" id="top" aria-labelledby="hero-title">
  <div class="hero-backdrop" aria-hidden="true"></div>
  <div class="wrap hero-copy">
    <a class="release-pill" href="{esc(channel.notes_url)}">
      <span class="release-pill-tag">{channel_name}</span>
      <span>{esc(channel.tag)}{date}</span>
      <span class="release-pill-link">Release notes {ICON_ARROW}</span>
    </a>
    <h1 id="hero-title">Team chat your server can&rsquo;t read.</h1>
    <p class="hero-lead">TeamCraft is a self-hosted team chat with channels, threads, calls and meetings.
    Everything is end-to-end encrypted on your devices, so the server you run it on never sees your
    messages, files or calls.</p>
    <div class="cta" data-channel="{esc(channel.name)}" data-channel-title="{channel_name}" data-app-store="{esc(maybe_app_store_url)}">
      <a class="button button-primary button-large" id="cta-button" href="#download">{ICON_DOWNLOAD}<span id="cta-label">Download TeamCraft</span></a>
      <a class="button button-secondary button-large" href="#download">Other platforms</a>
    </div>
    <p class="cta-meta" id="cta-meta">Windows, macOS, Linux and Android · Free and open source</p>
  </div>
  <div class="wrap showcase-wrap">
    {part("showcase")}
  </div>
</section>"""


def render_features(channel, env):
    features = [
        ("e2ee", "End-to-end encrypted", "Private by construction, not by promise.",
         """Messages, files, reactions, calls and search are encrypted on your devices with MLS and
         per-device keys. The server stores and relays ciphertext; even reaction counts stay hidden.""",
         ["Every message signed by the sending device",
          "Search runs on your devices over an encrypted index",
          "New devices are approved from one you already have"],
         part("visual-e2ee")),
        ("server", "Self-hosted", "Your server, and it doesn&rsquo;t have to be trusted.",
         """Run TeamCraft on a small virtual server with one container setup. It is designed for an
         untrusted host: whoever runs the machine sees who talks when, not what is said.""",
         ["Your identity provider (OpenID Connect) for sign-in",
          "Roles and policies signed by the workspace owner",
          "Admins manage people and channels without reading them"],
         part("visual-server")),
        ("calls", "Calls and meetings", "Calls that are as private as your chat.",
         """Voice and video calls, screen sharing and quick huddles in any channel. Every frame is
         encrypted on the sender&rsquo;s device; the media relay only forwards what it can&rsquo;t open.""",
         ["Scheduled meetings with invitations",
          "Guests join through a lobby you control",
          "A short code to compare encryption in the call"],
         part("visual-calls")),
        ("apps", "Apps everywhere", "On every device your team already uses.",
         """Desktop apps for Windows, macOS and Linux, an Android app and a web app served by your own
         server. Sign in once per device; your history comes along.""",
         ["Desktop apps update themselves with signed updates",
          "Linux packages through apt, dnf or zypper",
          "Android with or without Google services"],
         render_apps_visual(channel, env)),
    ]
    rows = "\n".join(render_feature(i, *feature) for i, feature in enumerate(features))
    return f"""<section class="section features" id="features" aria-labelledby="features-title">
  <div class="wrap">
    <header class="section-head section-head-center">
      <p class="eyebrow">Why TeamCraft</p>
      <h2 id="features-title">Everything a team needs to talk, nothing a server needs to read.</h2>
    </header>
{rows}
  </div>
</section>"""


def render_feature(index, slug, eyebrow, title, text, points, visual):
    items = "".join(f"<li>{ICON_CHECK}<span>{point}</span></li>" for point in points)
    flipped = " feature-flipped" if index % 2 else ""
    return f"""    <article class="feature{flipped} reveal" id="feature-{slug}" aria-labelledby="feature-{slug}-title">
      <div class="feature-copy">
        <p class="eyebrow">{eyebrow}</p>
        <h3 id="feature-{slug}-title">{title}</h3>
        <p>{text}</p>
        <ul class="checks">{items}</ul>
      </div>
      <div class="feature-visual">
        {visual}
      </div>
    </article>"""


def render_apps_visual(channel, env):
    """Platform tiles from the release's assets, plus the web app and the App Store when listed."""
    tiles = []
    for os_name, title in OS_TITLES.items():
        if any(d.os == os_name for d in channel.downloads):
            tiles.append((os_name, title, "Download", ""))
    tiles.append(("web", "Web", "Served by your server", ""))
    if env.get("APP_STORE_URL"):
        tiles.append(("ios", "iPhone, iPad", "App Store", ""))
    else:
        tiles.append(("ios", "iPhone, iPad", "Coming soon", " is-muted"))
    items = "".join(
        f'<li class="app-tile{muted}">{PLATFORM_ICONS[key]}<strong>{title}</strong><span>{status}</span></li>'
        for key, title, status, muted in tiles)
    return f"""<div class="visual visual-apps" role="img" aria-label="TeamCraft apps for {esc(", ".join(t[1] for t in tiles))}, and a new device being linked with a QR code">
  <ul class="app-tiles" aria-hidden="true">{items}</ul>
  <div class="link-card" aria-hidden="true">
    {pseudo_qr_code()}
    <div class="link-card-text">
      <strong>Link a new device</strong>
      <span>Scan with a device you&rsquo;re signed in on. Keys and history move over an encrypted channel.</span>
      <span class="link-card-status"><span class="dot"></span>Waiting for approval</span>
    </div>
  </div>
</div>"""


def pseudo_qr_code(size=25, seed=7):
    """A decorative QR-like pattern (not a readable code), the same on every build."""
    rng = random.Random(seed)
    finders = [(0, 0), (size - 7, 0), (0, size - 7)]

    def in_finder(x, y):
        return any(fx - 1 <= x <= fx + 7 and fy - 1 <= y <= fy + 7 for fx, fy in finders)

    cells = [(x, y) for y in range(size) for x in range(size) if not in_finder(x, y) and rng.random() < 0.48]
    rects = "".join(f'<rect x="{x}" y="{y}" width="1" height="1"/>' for x, y in cells)
    for fx, fy in finders:
        rects += (f'<path d="M{fx} {fy}h7v7h-7zM{fx + 1} {fy + 1}v5h5v-5z" fill-rule="evenodd"/>'
                  f'<rect x="{fx + 2}" y="{fy + 2}" width="3" height="3"/>')
    return f'<svg class="qr" viewBox="-2 -2 {size + 4} {size + 4}" aria-hidden="true">{rects}</svg>'


def render_downloads(maybe_stable, maybe_prerelease, env, has_linux_repositories):
    parts = []
    if maybe_stable:
        parts.append(render_channel_downloads(maybe_stable, env, has_linux_repositories))
    else:
        parts.append(f"""<div class="notice">
      {ICON_INFO}
      <p><strong>The first stable release is coming soon.</strong> Until then you can try the newest
      release candidate from the <a href="#channels">beta channel</a>. It is feature complete but may
      still have rough edges.</p>
    </div>""")
    if maybe_prerelease and (not maybe_stable or maybe_prerelease.tag != maybe_stable.tag):
        if maybe_stable:
            parts.append(f"""<details class="channel-more">
      <summary>Try the beta: {esc(maybe_prerelease.tag)} {prerelease_badge()}</summary>
      {render_channel_downloads(maybe_prerelease, env, has_linux_repositories)}
    </details>""")
        else:
            parts.append(render_channel_downloads(maybe_prerelease, env, has_linux_repositories))
    downloads = "\n    ".join(parts)
    return f"""<section class="section section-tinted" id="download" aria-labelledby="download-title">
  <div class="wrap">
    <header class="section-head">
      <p class="eyebrow">Download</p>
      <h2 id="download-title">Get TeamCraft for your devices.</h2>
      <p class="section-lead">Free and open source. The apps connect to the TeamCraft server of your
      organization; the web app is served by that server, no download needed.</p>
    </header>
    {downloads}
  </div>
</section>"""


def render_channel_downloads(channel, env, has_linux_repositories):
    badge = prerelease_badge() if channel.is_prerelease else '<span class="badge badge-stable">Stable</span>'
    date = ""
    if channel.published:
        date = (f'<span class="channel-sep" aria-hidden="true">·</span><time datetime="{esc(channel.published)}">'
                f'{esc(format_date(channel.published))}</time>')
    cards = "\n".join(render_platform(channel, os_name, env, has_linux_repositories) for os_name in OS_TITLES)
    return f"""<div class="channel" data-channel-block="{esc(channel.name)}">
      <p class="channel-head">{badge}<strong>{esc(channel.title)}</strong>{date}<span class="channel-sep" aria-hidden="true">·</span><a href="{esc(channel.notes_url)}">Release notes</a></p>
      <div class="platforms">
{cards}
      </div>
    </div>"""


def render_platform(channel, os_name, env, has_linux_repositories):
    downloads = [d for d in channel.downloads if d.os == os_name]
    primaries = [d for d in downloads if d.is_primary]
    others = [d for d in downloads if not d.is_primary]
    body = []
    if primaries and os_name in ARCH_CHOICES:
        body.append(render_arch_choice(primaries, channel, ARCH_CHOICES[os_name]))
    elif primaries:
        body.append(render_primary_downloads(primaries, channel))
    if any(d.is_test_build for d in downloads):
        body.append(f"""<p class="caution">{ICON_INFO}<span>Test build, signed with a throwaway key: it can&rsquo;t
          replace an install from a store, and the signed release will need a fresh install.</span></p>""")
    if os_name == "android" and env.get("PLAY_STORE_URL"):
        body.append(f'<a class="button button-secondary store" href="{esc(env["PLAY_STORE_URL"])}">Get it on Google Play</a>')
    if others or (os_name == "linux" and downloads and has_linux_repositories):
        body.append(render_more_downloads(others, channel, os_name == "linux" and has_linux_repositories))
    if not body:
        body.append('<p class="platform-empty">Not part of this release.</p>')
    content = "\n        ".join(body)
    maybe_summary = platform_summary(os_name, downloads) if downloads else ""
    summary = f'<p class="platform-sub">{esc(maybe_summary)}</p>' if maybe_summary else ""
    return f"""        <article class="platform" data-os="{os_name}" aria-labelledby="platform-{esc(channel.name)}-{os_name}">
          <div class="platform-head">
            <span class="platform-icon">{PLATFORM_ICONS[os_name]}</span>
            <div>
              <h3 id="platform-{esc(channel.name)}-{os_name}">{OS_TITLES[os_name]}</h3>
              {summary}
            </div>
            <span class="detected" hidden>Your system</span>
          </div>
          {content}
        </article>"""


def platform_summary(os_name, downloads):
    """Requirements and architectures, from the assets."""
    archs = list(dict.fromkeys(arch_title(d) for d in downloads if d.arch))
    parts = [OS_REQUIREMENTS[os_name]] if os_name in OS_REQUIREMENTS else []
    if archs:
        parts.append(" and ".join(archs) if len(archs) <= 2 else ", ".join(archs))
    primary_notes = [d.note for d in downloads if d.is_primary and d.note]
    if primary_notes and not archs:
        parts.append(primary_notes[0])
    return " · ".join(parts)


def render_primary_downloads(primaries, channel):
    """The main button (first architecture) and a link per further architecture."""
    first, rest = primaries[0], primaries[1:]
    meta = download_meta(first)
    button = (f'<a class="button button-primary download" href="{esc(first.url)}" {download_data(first, channel)} '
              f'aria-label="Download {esc(first.label)}, {esc(meta)}">{ICON_DOWNLOAD}'
              f'<span>Download {KIND_BUTTONS.get(first.kind, first.label)}</span></a>')
    others = ""
    if rest:
        links = " · ".join(
            f'<a class="download" href="{esc(d.url)}" {download_data(d, channel)} '
            f'aria-label="{esc(d.label)}, {esc(download_meta(d))}">{esc(download_meta(d))}</a>' for d in rest)
        others = f'<p class="primary-alt">Also for {links}</p>'
    return f"""<div class="primary">
            {button}
            <span class="primary-meta">{esc(meta)}</span>
          </div>
          {others}""".rstrip()


def render_arch_choice(primaries, channel, choice):
    """A button per architecture, the first one (the most common) highlighted, and how to tell which
    one the visitor's machine has. site.js may move the highlight; both stay visible."""
    buttons = []
    for index, download in enumerate(primaries):
        style = "button-primary" if index == 0 else "button-secondary"
        maybe_hint = choice["hints"].get(download.arch, "")
        hint = f'<span class="choice-hint">{esc(maybe_hint)}</span>' if maybe_hint else ""
        buttons.append(
            f'<a class="button {style} download choice" href="{esc(download.url)}" {download_data(download, channel)} '
            f'aria-label="Download {esc(download.label)} for {esc(download_meta(download))}">{ICON_DOWNLOAD}'
            f'<span class="choice-text"><span class="choice-title">{esc(arch_title(download))}</span>{hint}</span>'
            f'<span class="choice-size">{esc(format_size(download.size_bytes))}</span></a>')
    return f"""<div class="choices">
            {"".join(buttons)}
          </div>
          <p class="choice-help">{ICON_INFO}<span>{choice["help"]}</span></p>"""


def render_more_downloads(others, channel, links_repositories):
    """Every further package kind, with a link per architecture."""
    kinds = list(dict.fromkeys(d.kind for d in others))
    rows = []
    for kind in kinds:
        group = [d for d in others if d.kind == kind]
        first = group[0]
        note = f'<span class="option-note">{esc(first.note)}</span>' if first.note else ""
        links = "".join(
            f'<a class="option-link download" href="{esc(d.url)}" {download_data(d, channel)} '
            f'aria-label="{esc(d.label)}, {esc(download_meta(d))}">{esc(download_meta(d))}</a>' for d in group)
        rows.append(f'<li><span class="option-text"><span class="option-label">{esc(first.label)}</span>{note}</span>'
                    f'<span class="option-links">{links}</span></li>')
    if links_repositories:
        rows.append('<li class="option-repo"><span class="option-text"><span class="option-label">Package repository'
                    '</span><span class="option-note">Updates with your system through apt, dnf or zypper</span></span>'
                    f'<span class="option-links"><a class="option-link" href="#linux">Set up {ICON_ARROW}</a></span></li>')
    count = len(kinds) + (1 if links_repositories else 0)
    return f"""<details class="more">
            <summary><span>More options</span><span class="more-count">{count}</span>{ICON_CHEVRON}</summary>
            <ul class="options">
              {"".join(rows)}
            </ul>
          </details>"""


def download_meta(download):
    return " · ".join(x for x in (arch_title(download), format_size(download.size_bytes)) if x)


def arch_title(download):
    titles = OS_ARCH_TITLES.get(download.os, ARCH_TITLES)
    return titles.get(download.arch, download.arch)


def download_data(download, channel):
    """The attributes site.js reads to offer this download in the hero."""
    return (f'data-os="{download.os}" data-arch="{esc(download.arch)}" data-kind="{download.kind}" '
            f'data-label="{esc(download.label)}" data-meta="{esc(download_meta(download))}" '
            f'data-tag="{esc(channel.tag)}"' + (" data-primary" if download.is_primary else ""))


def render_channels(maybe_stable, maybe_prerelease):
    if maybe_stable:
        stable_text = f"""Tested releases for everyday use. The newest is
        <a href="{esc(maybe_stable.notes_url)}">{esc(maybe_stable.tag)}</a>."""
    else:
        stable_text = """Tested releases for everyday use. The first stable release is coming soon;
        until then, the beta channel is the way to try TeamCraft."""
    if maybe_prerelease:
        beta_text = f"""Release candidates as soon as they are published
        (<a href="{esc(maybe_prerelease.notes_url)}">{esc(maybe_prerelease.tag)}</a> today), and every
        stable release as well, so beta is never behind stable."""
    else:
        beta_text = """Release candidates as soon as they are published, and every stable release as
        well, so beta is never behind stable."""
    recommended = "Recommended" if maybe_stable else "Coming soon"
    return f"""<section class="section" id="channels" aria-labelledby="channels-title">
  <div class="wrap">
    <header class="section-head">
      <p class="eyebrow">Release channels</p>
      <h2 id="channels-title">Stable or Beta, your choice.</h2>
      <p class="section-lead">Installed desktop apps follow the channel your organization sets in the
      admin console (stable by default) and update themselves within it. Linux repositories have a
      suite per channel.</p>
    </header>
    <div class="channel-cards">
      <article class="channel-card reveal">
        <h3><span class="badge badge-stable">Stable</span><span class="channel-card-note">{recommended}</span></h3>
        <p>{stable_text}</p>
      </article>
      <article class="channel-card reveal">
        <h3>{prerelease_badge()}<span class="channel-card-note">Early access</span></h3>
        <p>{beta_text}</p>
      </article>
    </div>
  </div>
</section>"""


def render_linux_repositories(channels, main_channel, site, site_url):
    """Stable and beta (the prerelease channel; its paths keep the name "prerelease") as tabs."""
    blocks = {name: render_repository_channel(c, site, site_url) for name, c in channels.items()}
    if not any(blocks.values()):
        return ""
    stable = blocks.get("stable") or """<p class="hint">The first stable release is coming soon. Until
        then, use the beta channel; it receives the stable releases too.</p>"""
    panels = [("stable", "Stable", stable)]
    if blocks.get("prerelease"):
        beta = f"""<p class="hint">Release candidates as soon as they are published
        ({esc(channels["prerelease"].tag)} today), and every stable release as well.</p>
        {blocks["prerelease"]}
        {render_switch_to_stable()}"""
        panels.append(("prerelease", "Beta", beta))
    # Without script every panel shows with its heading; site.js turns the headings into tabs.
    selected = main_channel.name if any(name == main_channel.name for name, _, _ in panels) else panels[0][0]
    rendered = "\n".join(f"""      <div class="tab-panel" id="linux-{name}" data-tab-title="{title}"{" data-selected" if name == selected else ""}>
        <h3 class="tab-panel-title">{title}{" " + prerelease_badge() if name == "prerelease" else ""}</h3>
        {content}
      </div>""" for name, title, content in panels)
    return f"""<section class="section section-tinted" id="linux" aria-labelledby="linux-title">
  <div class="wrap">
    <header class="section-head">
      <p class="eyebrow">Linux</p>
      <h2 id="linux-title">Updates with the rest of your system.</h2>
      <p class="section-lead">Add the package repository and TeamCraft updates with your system. Packages
      and repository metadata are signed with the <a href="#verify-key">repository key</a>.</p>
    </header>
    <div class="tabs" data-tabs>
{rendered}
    </div>
  </div>
</section>"""


def render_switch_to_stable():
    """Back from beta to stable: the APT sources file is shared, the rpm repositories have own files."""
    text = """# APT: point the source at the stable suite
sudo sed -i 's/^Suites: prerelease$/Suites: stable/' /etc/apt/sources.list.d/teamcraft.sources
# DNF: remove the beta repository, then add the stable one
sudo rm /etc/yum.repos.d/teamcraft-prerelease.repo
# zypper: remove the beta repository, then add the stable one
sudo zypper removerepo teamcraft-prerelease"""
    return f"""<details class="switch">
          <summary>Switch back from beta to stable {ICON_CHEVRON}</summary>
          {render_snippet("switch-stable", "Leave the beta channel", text)}
          <p class="hint">The installed beta version stays until a newer stable version is published; to go
          back right away, reinstall the package after switching.</p>
        </details>"""


def render_repository_channel(channel, site, site_url):
    has_apt = (site / "apt" / "dists" / channel.name / "InRelease").is_file()
    repo_name = "teamcraft" if channel.name == "stable" else f"teamcraft-{channel.name}"
    has_rpm = (site / "rpm" / channel.name / f"{repo_name}.repo").is_file()
    if not has_apt and not has_rpm:
        return ""
    parts = ['<div class="snippets">']
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
    parts.append(f'<p class="hint files">Files: {" · ".join(links)}</p>')
    return "\n        ".join(parts)


def render_snippet(snippet_id, title, text, is_wide=False):
    wide = " snippet-wide" if is_wide else ""
    return f"""<figure class="snippet{wide}">
          <figcaption><span>{esc(title)}</span>
          <button type="button" class="copy" data-copy="{esc(snippet_id)}" hidden>{ICON_COPY}<span>Copy</span></button></figcaption>
          <pre id="{esc(snippet_id)}"><code>{esc(text)}</code></pre>
        </figure>"""


def render_verify(channels, site, repo, env):
    cards = []
    checksum_links = " · ".join(
        f'<a href="{esc(c.maybe_checksums_url)}">SHA256SUMS of {esc(c.tag)}</a>'
        for c in channels.values() if c.maybe_checksums_url)
    if checksum_links:
        cards.append(f"""<article class="verify-card reveal">
        <span class="verify-icon">{ICON_HASH}</span>
        <h3>Checksums</h3>
        <p>Every release lists the SHA-256 of its files: {checksum_links}. Download it next to the file
        and run:</p>
        {render_snippet("verify-sha", "Linux, macOS", "sha256sum --check --ignore-missing SHA256SUMS")}
        {render_snippet("verify-sha-win", "Windows (PowerShell)", "Get-FileHash .\\TeamCraft_*-setup.exe -Algorithm SHA256")}
      </article>""")
    cards.append(f"""<article class="verify-card reveal">
        <span class="verify-icon">{ICON_PROVENANCE}</span>
        <h3>Build provenance</h3>
        <p>Release files are built by GitHub Actions from the tagged source and carry a signed build
        provenance attestation. Check one with the GitHub CLI:</p>
        {render_snippet("verify-attestation", "Any system", f"gh attestation verify <file> --repo {repo}")}
      </article>""")
    fingerprint = re.sub(r"\s+", "", env.get("GPG_KEY_ID", "")).upper()
    if fingerprint and (site / "teamcraft-archive-keyring.asc").is_file():
        groups = [fingerprint[i:i + 4] for i in range(0, len(fingerprint), 4)]
        half = (len(groups) + 1) // 2
        grouped = f'{" ".join(groups[:half])} <wbr>{" ".join(groups[half:])}'
        cards.append(f"""<article class="verify-card reveal" id="verify-key">
        <span class="verify-icon">{ICON_KEY}</span>
        <h3>Repository key</h3>
        <p>APT and RPM metadata and the rpm packages on this site are signed with the
        <a href="teamcraft-archive-keyring.asc">repository key</a>. Its fingerprint:</p>
        <p class="fingerprint"><code>{grouped}</code></p>
        {render_snippet("verify-key-cmd", "Check a downloaded key", "gpg --show-keys teamcraft-archive-keyring.asc")}
        <p class="hint">The rpms here are signed copies, so their checksums differ from the release&rsquo;s
        SHA256SUMS.</p>
      </article>""")
    cards.append(f"""<article class="verify-card reveal">
        <span class="verify-icon">{ICON_SHIELD}</span>
        <h3>Desktop updates</h3>
        <p>Installed desktop apps update themselves only with packages signed by the TeamCraft updater
        key; the app checks the signature before installing. A mirror can hold an update back, never
        slip one in.</p>
      </article>""")
    return f"""<section class="section" id="verify" aria-labelledby="verify-title">
  <div class="wrap">
    <header class="section-head">
      <p class="eyebrow">Verify</p>
      <h2 id="verify-title">Check what you install.</h2>
      <p class="section-lead">Every file comes from a public build of the tagged source. Here is how to
      confirm the one you downloaded is that file.</p>
    </header>
    <div class="verify-grid">
      {"".join(cards)}
    </div>
  </div>
</section>"""


def render_self_hosting(repo_url):
    return f"""<section class="section self-hosting" id="self-hosting" aria-labelledby="self-hosting-title">
  <div class="wrap">
    <div class="self-hosting-panel reveal">
      <div class="self-hosting-copy">
        <p class="eyebrow">Self-hosting</p>
        <h2 id="self-hosting-title">Run your own server.</h2>
        <p>The apps connect to a TeamCraft server run by your organization. The deployment repository
        has a ready Docker Compose setup with the server, media relay, identity provider and database,
        plus upgrade and backup guides.</p>
        <div class="self-hosting-links">
          <a class="button button-light" href="https://github.com/{DEPLOY_REPO}">Deployment guide {ICON_ARROW}</a>
          <a class="button button-ghost-light" href="{esc(repo_url)}/blob/main/docs/operations.md">Operations docs</a>
        </div>
      </div>
      <ul class="stack" aria-label="What the setup runs">
        <li><strong>Server</strong><span>REST API and real-time gateway</span></li>
        <li><strong>Media relay</strong><span>Forwards encrypted call media</span></li>
        <li><strong>Identity provider</strong><span>Sign-in with OpenID Connect</span></li>
        <li><strong>Database</strong><span>PostgreSQL, ciphertext and metadata</span></li>
      </ul>
    </div>
  </div>
</section>"""


def render_footer(channels, site, repo_url, has_linux_repositories):
    manifests = "".join(
        f'<li><a href="updates/{name}/latest.json">Update manifest ({"beta" if name == "prerelease" else name})</a></li>'
        for name in channels if (site / "updates" / name / "latest.json").is_file())
    linux = '<li><a href="#linux">Linux repositories</a></li>' if has_linux_repositories else ""
    return f"""<footer class="site-footer">
  <div class="wrap footer-grid">
    <div class="footer-brand">
      <a class="brand" href="#top"><img src="logo.png" alt="" width="24" height="24"><span>TeamCraft</span></a>
      <p>Self-hosted, end-to-end encrypted team chat, calls and meetings.</p>
    </div>
    <nav aria-label="Downloads">
      <h2>Downloads</h2>
      <ul>
        <li><a href="#download">All platforms</a></li>
        {linux}
        <li><a href="#verify">Verify a download</a></li>
        {manifests}
      </ul>
    </nav>
    <nav aria-label="Project">
      <h2>Project</h2>
      <ul>
        <li><a href="{esc(repo_url)}">Source code</a></li>
        <li><a href="{esc(repo_url)}/releases">All releases</a></li>
        <li><a href="{esc(repo_url)}/blob/main/SECURITY.md">Security policy</a></li>
        <li><a href="{esc(repo_url)}/blob/main/docs/updates.md">Packaging docs</a></li>
        <li><a href="https://github.com/{DEPLOY_REPO}">Deployment</a></li>
      </ul>
    </nav>
  </div>
  <div class="wrap footer-legal">
    <p>Free software under the <a href="{esc(repo_url)}/blob/main/LICENSE">GNU AGPL-3.0</a>.
    No cookies, no trackers. Fonts: Inter and JetBrains Mono (<a href="fonts/OFL.txt">SIL Open Font License</a>).</p>
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


def part(name):
    """A fixed illustration from scripts/site-parts/, without its leading comment."""
    text = (PARTS_DIR / f"{name}.html").read_text(encoding="utf-8")
    return re.sub(r"^\s*<!--.*?-->\s*", "", text, flags=re.DOTALL).strip()


def prerelease_badge():
    return '<span class="badge badge-beta">Beta</span>'


def format_size(size_bytes):
    if size_bytes <= 0:
        return ""
    return f"{size_bytes / 1_000_000:.0f} MB" if size_bytes >= 1_000_000 else f"{size_bytes / 1000:.0f} kB"


def format_date(iso_date):
    """2026-10-23 -> 23 Oct 2026 (the same in every locale); anything else stays as it is."""
    try:
        date = datetime.date.fromisoformat(iso_date)
    except ValueError:
        return iso_date
    return f"{date.day} {MONTHS[date.month - 1]} {date.year}"


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
        broken += broken_targets(page, targets, site, site_url)
    for sheet in sorted(site.rglob("*.css")):
        urls = re.findall(r"url\(\s*['\"]?([^'\")]+)['\"]?\s*\)", sheet.read_text(encoding="utf-8"))
        broken += broken_targets(sheet, [(u, False) for u in urls], site, site_url)
    return broken


def broken_targets(page, targets, site, site_url):
    broken = []
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


def icon(paths, class_name="icon"):
    """A 24 px line icon (stroke follows the text colour)."""
    return f'<svg class="{class_name}" viewBox="0 0 24 24" aria-hidden="true" focusable="false">{paths}</svg>'


# Simple line icons, no vendor logos.
PLATFORM_ICONS = {
    "windows": icon('<rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>'),
    "macos": icon('<rect x="4" y="5" width="16" height="11" rx="2"/><path d="M2 19h20"/>'),
    "linux": icon('<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 9l3 3-3 3M12 15h5"/>'),
    "android": icon('<rect x="6" y="2" width="12" height="20" rx="2.5"/><path d="M10.5 18.5h3"/>'),
    "web": icon('<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9S9.5 5.6 12 3z"/>'),
    "ios": icon('<rect x="5" y="2" width="14" height="20" rx="3"/><path d="M10 5h4"/>'),
}
ICON_DOWNLOAD = icon('<path d="M12 4v11M7 10l5 5 5-5M5 20h14"/>')
ICON_ARROW = icon('<path d="M5 12h14M13 6l6 6-6 6"/>', "icon icon-inline")
ICON_CHEVRON = icon('<path d="M6 9l6 6 6-6"/>', "icon chevron")
ICON_CHECK = icon('<path d="M5 12.5l4.5 4.5L19 7.5"/>', "icon check")
ICON_INFO = icon('<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.5v.5"/>')
ICON_COPY = icon('<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>')
ICON_HASH = icon('<path d="M5 9h15M4 15h15M10 3 8 21M16 3l-2 18"/>')
ICON_PROVENANCE = icon('<path d="M12 3l7 4v5c0 4.2-2.9 7.7-7 9-4.1-1.3-7-4.8-7-9V7z"/><path d="M8.5 12l2.5 2.5 4.5-5"/>')
ICON_KEY = icon('<circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M17 6l3 3M14 9l2 2"/>')
ICON_SHIELD = icon('<path d="M12 3l7 4v5c0 4.2-2.9 7.7-7 9-4.1-1.3-7-4.8-7-9V7z"/><path d="M12 8v5M12 16v.5"/>')

PAGE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="{csp}">
<meta name="referrer" content="no-referrer">
<meta name="color-scheme" content="light dark">
<meta name="theme-color" content="#ffffff" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#0b0f16" media="(prefers-color-scheme: dark)">
<meta name="description" content="TeamCraft: self-hosted, end-to-end encrypted team chat, calls and meetings. Downloads for Windows, macOS, Linux and Android.">
<title>TeamCraft – encrypted team chat, calls and meetings</title>
<link rel="icon" href="favicon.png" type="image/png">
<link rel="apple-touch-icon" href="apple-touch-icon.png">
<link rel="preload" href="fonts/inter-latin-variable.woff2" as="font" type="font/woff2" crossorigin>
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
