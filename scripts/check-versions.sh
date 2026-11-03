#!/usr/bin/env bash
# Checks that every place carrying the release version agrees (docs/release.md, "Versions"):
# Cargo.toml (workspace), apps/desktop/tauri.conf.json, the Android versionName and versionCode
# (formula below) and the iOS MARKETING_VERSION / CURRENT_PROJECT_VERSION. CI runs it on every push.
#
#   scripts/check-versions.sh            the tree is consistent; prints the version
#   scripts/check-versions.sh v1.2.3     ... and the tag fits it (release workflow)
#
# A tag fits when it equals the version (v1.2.3 for 1.2.3) or is a prerelease of it (v1.2.3-rc.1 for
# 1.2.3: a candidate built from the release tree).
# With GITHUB_OUTPUT set, writes version, tree_version and is_prerelease to it.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "error: $*" >&2; [ -n "${GITHUB_ACTIONS:-}" ] && echo "::error::$*"; exit 1; }

cargo="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
desktop="$(sed -n 's/^  "version": "\(.*\)",$/\1/p' apps/desktop/tauri.conf.json)"
android_name="$(sed -n 's/^ *versionName = "\(.*\)"$/\1/p' apps/android/app/build.gradle.kts)"
android_code="$(sed -n 's/^ *versionCode = \([0-9]*\)$/\1/p' apps/android/app/build.gradle.kts)"
ios_marketing="$(sed -n 's/^ *MARKETING_VERSION: "\(.*\)"$/\1/p' apps/ios/project.yml)"
ios_build="$(sed -n 's/^ *CURRENT_PROJECT_VERSION: "\(.*\)"$/\1/p' apps/ios/project.yml)"

[[ "$cargo" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-(beta|rc)\.([0-9]+))?$ ]] \
  || fail "Cargo.toml version '$cargo' is not MAJOR.MINOR.PATCH[-beta.N|-rc.N]"
major="${BASH_REMATCH[1]}" minor="${BASH_REMATCH[2]}" patch="${BASH_REMATCH[3]}"
stage_kind="${BASH_REMATCH[5]}" stage_number="${BASH_REMATCH[6]}"
base="$major.$minor.$patch"

# versionCode = MAJOR*1000000 + MINOR*10000 + PATCH*100 + stage; stage: beta.N -> N, rc.N -> 50+N,
# release -> 99. Strictly increasing from one release to the next, as Google Play requires.
case "$stage_kind" in
  beta) stage="$stage_number" ;;
  rc) stage=$((50 + stage_number)) ;;
  *) stage=99 ;;
esac
code=$((major * 1000000 + minor * 10000 + patch * 100 + stage))

[ "$desktop" = "$cargo" ] || fail "tauri.conf.json has $desktop, Cargo.toml $cargo"
[ "$android_name" = "$cargo" ] || fail "Android versionName is $android_name, Cargo.toml $cargo"
[ "$android_code" = "$code" ] || fail "Android versionCode is $android_code, $cargo needs $code"
# App Store versions are numeric only; the build number follows the Android code.
[ "$ios_marketing" = "$base" ] || fail "iOS MARKETING_VERSION is $ios_marketing, $cargo needs $base"
[ "$ios_build" = "$code" ] || fail "iOS CURRENT_PROJECT_VERSION is $ios_build, $cargo needs $code"

version="$cargo"
if [ $# -gt 0 ]; then
  tag="${1#v}"
  if [ "$tag" != "$cargo" ]; then
    case "$tag" in
      "$cargo"-*) [ -z "$stage_kind" ] || fail "tag v$tag: a candidate tag needs a release version in the tree, not $cargo" ;;
      *) fail "tag v$tag does not match the tree version $cargo" ;;
    esac
  fi
  version="$tag"
fi
case "$version" in *-*) is_prerelease=true ;; *) is_prerelease=false ;; esac

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "version=$version"
    echo "tree_version=$cargo"
    echo "is_prerelease=$is_prerelease"
  } >> "$GITHUB_OUTPUT"
fi
echo "$version"
