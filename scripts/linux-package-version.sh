#!/usr/bin/env bash
# Linux package versions for a release version (docs/release.md, "Versions"). Tauri names its deb and
# rpm after the tree's version (1.0.0), so a candidate would carry the final version and dnf/apt would
# never upgrade between candidates or from a candidate to the release. This maps a release version
# (1.0.0-rc.2) to versions that sort before the final one:
#
#   scripts/linux-package-version.sh rpm-release 1.0.0-rc.2   -> 0.rc.2     (release: 1)
#   scripts/linux-package-version.sh deb-version 1.0.0-rc.2   -> 1.0.0~rc.2 (release: 1.0.0)
#   scripts/linux-package-version.sh repack-deb 1.0.0-rc.2 FILE.deb   sets the deb's version, renames it
#   scripts/linux-package-version.sh check [VERSION]          candidates sort before the release (CI)
#
# The rpm keeps Version 1.0.0 and gets Release 0.rc.2 (bundle.linux.rpm.release; Tauri has no
# per-package version override and the config version must stay semver). The update manifest
# (latest.json) and the app keep the semver version (1.0.0-rc.2).
set -euo pipefail

fail() { echo "error: $*" >&2; [ -n "${GITHUB_ACTIONS:-}" ] && echo "::error::$*"; exit 1; }

# Splits MAJOR.MINOR.PATCH[-beta.N|-rc.N] into $base and $stage ("" for a release).
split_version() {
  [[ "$1" =~ ^([0-9]+\.[0-9]+\.[0-9]+)(-((beta|rc)\.[0-9]+))?$ ]] || fail "unsupported version '$1'"
  base="${BASH_REMATCH[1]}"
  stage="${BASH_REMATCH[3]}"
}

rpm_release() {
  split_version "$1"
  # 0.<stage> sorts before the release's 1 (the Fedora pre-release convention).
  if [ -n "$stage" ]; then echo "0.$stage"; else echo 1; fi
}

rpm_version_release() {
  split_version "$1"
  echo "$base-$(rpm_release "$1")"
}

deb_version() {
  split_version "$1"
  # The tilde sorts before the empty string: 1.0.0~rc.2 < 1.0.0.
  if [ -n "$stage" ]; then echo "$base~$stage"; else echo "$base"; fi
}

# Rewrites the Version of a deb that Tauri built from the tree's version, and renames the file.
repack_deb() {
  local version="$1" deb="$2" new work name target
  new="$(deb_version "$version")"
  name="$(basename "$deb")"
  target="$(dirname "$deb")/$(echo "$name" | sed -E "s/_[^_]+_/_${new}_/")"
  work="$(mktemp -d)"
  dpkg-deb --raw-extract "$deb" "$work/root"
  sed -i "s/^Version: .*/Version: $new/" "$work/root/DEBIAN/control"
  grep -q "^Version: $new\$" "$work/root/DEBIAN/control" || fail "could not set the version in $deb"
  # gzip: readable by every apt/dpkg that reads Tauri's own debs.
  dpkg-deb --root-owner-group -Zgzip --build "$work/root" "$work/new.deb" >/dev/null
  rm -f "$deb"
  mv "$work/new.deb" "$target"
  rm -rf "$work"
}

rpm_compare() { rpm --eval "%{lua: print(rpm.vercmp('$1', '$2'))}"; }

# Asserts that release version $1 sorts before $2 for rpm (version-release) and deb.
assert_less() {
  local rpm_low rpm_high deb_low deb_high
  rpm_low="$(rpm_version_release "$1")" rpm_high="$(rpm_version_release "$2")"
  deb_low="$(deb_version "$1")" deb_high="$(deb_version "$2")"
  [ "$(rpm_compare "$rpm_low" "$rpm_high")" = -1 ] || fail "rpm $rpm_low does not sort before $rpm_high"
  dpkg --compare-versions "$deb_low" lt "$deb_high" || fail "deb $deb_low does not sort before $deb_high"
  echo "ok: $1 < $2 (rpm $rpm_low < $rpm_high, deb $deb_low < $deb_high)"
}

check() {
  assert_less 1.0.0-rc.1 1.0.0-rc.2
  assert_less 1.0.0-rc.2 1.0.0-rc.10
  assert_less 1.0.0-beta.3 1.0.0-rc.1
  assert_less 1.0.0-rc.11 1.0.0
  assert_less 1.0.0 1.0.1-rc.1
  assert_less 1.0.1-rc.1 1.0.1
  # The version being released, when it is a candidate, sorts before its release.
  if [ -n "${1:-}" ]; then
    split_version "${1#v}"
    if [ -n "$stage" ]; then assert_less "${1#v}" "$base"; fi
  fi
}

case "${1:-}" in
  rpm-release) rpm_release "${2:?version}" ;;
  deb-version) deb_version "${2:?version}" ;;
  repack-deb) repack_deb "${2:?version}" "${3:?deb file}" ;;
  check) check "${2:-}" ;;
  *) fail "usage: $0 rpm-release|deb-version VERSION | repack-deb VERSION FILE | check [VERSION]" ;;
esac
