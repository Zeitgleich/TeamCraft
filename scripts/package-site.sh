#!/usr/bin/env bash
# Builds the package site (docs/updates.md §5): update manifests per channel and signed APT and RPM
# repositories, from the published GitHub releases. The site has no state of its own; every run
# rebuilds it.
#
#   scripts/package-site.sh fetch <work>          newest stable and newest release of any kind into
#                                                 <work>/{stable,prerelease} (needs gh and REPO)
#   scripts/package-site.sh build <work> <site>   the site from <work> (needs apt-ftparchive,
#                                                 createrepo_c, rpmsign, gpg and the env below)
#
# Environment for build:
#   GPG_KEY_ID          fingerprint of the repository key, already imported into gpg; empty: the
#                       site gets the update manifests only, no APT/RPM repositories
#   GPG_PASSPHRASE_FILE file with its passphrase (may be empty for a key without one)
#   SITE_URL            where the site is served, e.g. https://example-org.github.io/TeamCraft
set -euo pipefail

fail() { echo "error: $*" >&2; [ -n "${GITHUB_ACTIONS:-}" ] && echo "::error::$*"; exit 1; }
CHANNELS=(stable prerelease)

main() {
  case "${1:-}" in
    fetch) [ $# -eq 2 ] || usage; fetch "$2" ;;
    build) [ $# -eq 3 ] || usage; build "$2" "$3" ;;
    *) usage ;;
  esac
}

usage() { fail "usage: $0 fetch <work> | build <work> <site>"; }

fetch() {
  local work="$1" channel tag
  : "${REPO:?REPO (owner/name) is required}"
  mkdir -p "$work"
  # Published releases only (drafts are invisible to `latest`): the newest stable one, and the newest
  # of any kind by semantic version, so prerelease users get stable releases too.
  gh api "repos/$REPO/releases" --paginate --jq '.[] | select(.draft | not) | [.tag_name, .prerelease] | @tsv' \
    > "$work/releases.tsv"
  read -r stable_tag prerelease_tag < <(python3 - "$work/releases.tsv" <<'PY'
import re, sys
def key(tag):
    match = re.match(r"^v(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$", tag)
    if not match:
        return None
    major, minor, patch, pre = match.groups()
    # Semantic version order: a release sorts after its prereleases; numeric parts compare as numbers.
    parts = [(0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split(".")] if pre else []
    return (int(major), int(minor), int(patch), 0 if pre else 1, parts)
releases = []
for line in open(sys.argv[1]):
    tag, prerelease = line.rstrip("\n").split("\t")
    if key(tag) is not None:
        releases.append((key(tag), tag, prerelease == "true"))
stable = max((r for r in releases if not r[2]), default=None)
newest = max(releases, default=None)
print(stable[1] if stable else "-", newest[1] if newest else "-")
PY
  )
  for channel in "${CHANNELS[@]}"; do
    tag="$stable_tag"
    [ "$channel" = prerelease ] && tag="$prerelease_tag"
    [ "$tag" != "-" ] || { echo "no $channel release"; continue; }
    echo "$channel: $tag"
    mkdir -p "$work/$channel"
    echo "$tag" > "$work/$channel/TAG"
    gh release download "$tag" --repo "$REPO" --dir "$work/$channel" --pattern '*.deb' --pattern '*.rpm' --clobber
    # Releases without updater signatures have no manifest; the channel then has packages only.
    gh release download "$tag" --repo "$REPO" --dir "$work/$channel" --pattern 'latest.json' --clobber \
      || echo "$tag has no latest.json"
  done
}

build() {
  local work site
  work="$(cd "$1" && pwd)"
  mkdir -p "$2"
  site="$(cd "$2" && pwd)"
  : "${SITE_URL:?SITE_URL is required}"
  SITE_URL="${SITE_URL%/}"
  touch "$site/.nojekyll"
  local has_repositories=0
  if [ -n "${GPG_KEY_ID:-}" ]; then
    : "${GPG_PASSPHRASE_FILE:?GPG_PASSPHRASE_FILE is required}"
    has_repositories=1
    gpg --batch --yes --armor --export "$GPG_KEY_ID" > "$site/teamcraft-archive-keyring.asc"
    gpg --batch --yes --export "$GPG_KEY_ID" > "$site/teamcraft-archive-keyring.gpg"
    [ -s "$site/teamcraft-archive-keyring.gpg" ] || fail "gpg has no key $GPG_KEY_ID"
  else
    echo "warning: no repository key: update manifests only, no APT/RPM repositories" >&2
    [ -n "${GITHUB_ACTIONS:-}" ] && echo "::warning::no repository key (LINUX_REPO_GPG_PRIVATE_KEY): no APT/RPM repositories"
  fi

  local channel built=0
  for channel in "${CHANNELS[@]}"; do
    [ -f "$work/$channel/TAG" ] || continue
    built=1
    if [ -f "$work/$channel/latest.json" ]; then
      mkdir -p "$site/updates/$channel"
      cp "$work/$channel/latest.json" "$site/updates/$channel/latest.json"
    fi
    if [ "$has_repositories" = 1 ]; then
      build_apt "$work/$channel" "$site/apt" "$channel"
      build_rpm "$work/$channel" "$site/rpm/$channel" "$channel"
    fi
  done
  [ "$built" = 1 ] || fail "no release to publish in $work"
  write_index "$site"
}

# One suite per channel; packages in pool/<tag>/ (a candidate and its release carry the same
# version number but are different builds).
build_apt() {
  local from="$1" apt="$2" suite="$3" tag arch
  tag="$(cat "$from/TAG")"
  mkdir -p "$apt/pool/$tag"
  find "$from" -maxdepth 1 -name '*.deb' -exec cp {} "$apt/pool/$tag/" \;
  for arch in amd64 arm64; do
    mkdir -p "$apt/dists/$suite/main/binary-$arch"
    (cd "$apt" && apt-ftparchive --arch "$arch" packages "pool/$tag") > "$apt/dists/$suite/main/binary-$arch/Packages"
    gzip -9 -n -k -f "$apt/dists/$suite/main/binary-$arch/Packages"
  done
  grep -q '^Package: ' "$apt/dists/$suite/main/binary-amd64/Packages" || fail "no amd64 deb for $suite ($tag)"
  apt-ftparchive \
    -o "APT::FTPArchive::Release::Origin=TeamCraft" \
    -o "APT::FTPArchive::Release::Label=TeamCraft" \
    -o "APT::FTPArchive::Release::Suite=$suite" \
    -o "APT::FTPArchive::Release::Codename=$suite" \
    -o "APT::FTPArchive::Release::Components=main" \
    -o "APT::FTPArchive::Release::Architectures=amd64 arm64" \
    release "$apt/dists/$suite" > "$apt/dists/$suite/Release.tmp"
  mv "$apt/dists/$suite/Release.tmp" "$apt/dists/$suite/Release"
  sign --clearsign --output "$apt/dists/$suite/InRelease" "$apt/dists/$suite/Release"
  sign --detach-sign --armor --output "$apt/dists/$suite/Release.gpg" "$apt/dists/$suite/Release"
}

# Signed packages (gpgcheck) and signed metadata (repo_gpgcheck).
build_rpm() {
  local from="$1" dir="$2" channel="$3"
  mkdir -p "$dir"
  find "$from" -maxdepth 1 -name '*.rpm' -exec cp {} "$dir/" \;
  compgen -G "$dir/*.rpm" >/dev/null || fail "no rpm for $channel"
  # rpm's default gpg path differs between distributions.
  rpmsign --addsign \
    --define "__gpg $(command -v gpg)" \
    --define "_gpg_name $GPG_KEY_ID" \
    --define "_gpg_sign_cmd_extra_args --batch --pinentry-mode loopback --passphrase-file $GPG_PASSPHRASE_FILE" \
    "$dir"/*.rpm >/dev/null
  createrepo_c --quiet "$dir"
  sign --detach-sign --armor --output "$dir/repodata/repomd.xml.asc" "$dir/repodata/repomd.xml"
  local name="teamcraft" title="TeamCraft"
  if [ "$channel" != stable ]; then name="teamcraft-$channel"; title="TeamCraft ($channel)"; fi
  cat > "$dir/$name.repo" <<EOF
[$name]
name=$title
baseurl=$SITE_URL/rpm/$channel
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=$SITE_URL/teamcraft-archive-keyring.asc
EOF
}

sign() {
  gpg --batch --yes --pinentry-mode loopback --passphrase-file "$GPG_PASSPHRASE_FILE" \
    --local-user "$GPG_KEY_ID" "$@"
}

write_index() {
  local site="$1"
  cat > "$site/index.html" <<EOF
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<title>TeamCraft packages</title>
<h1>TeamCraft packages</h1>
<p>APT and RPM repositories and the desktop update manifests of TeamCraft.
Setup: <a href="https://github.com/${REPO:-Zeitgleich/TeamCraft}/blob/main/docs/updates.md">docs/updates.md</a>.</p>
<ul>
<li><a href="teamcraft-archive-keyring.asc">Repository key</a></li>
<li><a href="rpm/stable/teamcraft.repo">teamcraft.repo</a> (Fedora, RHEL, openSUSE)</li>
<li>APT: <code>deb [signed-by=/usr/share/keyrings/teamcraft-archive-keyring.gpg] $SITE_URL/apt stable main</code></li>
</ul>
</html>
EOF
}

main "$@"
