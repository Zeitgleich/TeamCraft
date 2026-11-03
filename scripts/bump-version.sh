#!/usr/bin/env bash
# Sets the release version in every place that carries it (docs/release.md, "Versions") and
# verifies the result with scripts/check-versions.sh. Used by the nightly release
# (.github/workflows/nightly-release.yml) and for hand-made version bumps.
#
#   scripts/bump-version.sh 1.0.9
#   scripts/bump-version.sh 1.1.0 "Summary for the AppStream release entry."
#
# Only stable versions (MAJOR.MINOR.PATCH). Updates Cargo.lock with `cargo update --workspace`.
set -euo pipefail
cd "$(dirname "$0")/.."

[ $# -ge 1 ] || { echo "usage: $0 MAJOR.MINOR.PATCH [appstream-summary]" >&2; exit 2; }
version="$1"
summary="${2:-Regular update; see the release notes on GitHub for the changes.}"
[[ "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || { echo "error: '$version' is not MAJOR.MINOR.PATCH" >&2; exit 2; }
code=$((BASH_REMATCH[1] * 1000000 + BASH_REMATCH[2] * 10000 + BASH_REMATCH[3] * 100 + 99))
metainfo=apps/desktop/packaging/linux/de.zeitgleich.teamcraft.metainfo.xml

# sed -i.bak works the same with GNU and BSD sed.
replace() { # file, sed expression
  sed -i.bak -E "$2" "$1" && rm -f "$1.bak"
}

# Only the [workspace.package] version, not dependency versions.
replace Cargo.toml "/^\[workspace.package\]/,/^\[/ s/^version = \".*\"/version = \"$version\"/"
replace apps/desktop/tauri.conf.json "s/^  \"version\": \".*\",$/  \"version\": \"$version\",/"
replace apps/android/app/build.gradle.kts "s/^( *versionCode = )[0-9]+$/\1$code/; s/^( *versionName = )\".*\"$/\1\"$version\"/"
replace apps/ios/project.yml "s/^( *MARKETING_VERSION: )\".*\"$/\1\"$version\"/; s/^( *CURRENT_PROJECT_VERSION: )\".*\"$/\1\"$code\"/"

# AppStream: a new <release> on top, unless this version already has one.
if ! grep -q "<release version=\"$version\"" "$metainfo"; then
  VERSION="$version" DATE="$(date -u +%Y-%m-%d)" SUMMARY="$summary" METAINFO="$metainfo" python3 - <<'PY'
import os
import pathlib

path = pathlib.Path(os.environ["METAINFO"])
entry = (
    f'    <release version="{os.environ["VERSION"]}" date="{os.environ["DATE"]}" type="stable">\n'
    f"      <description>\n        <p>{os.environ['SUMMARY']}</p>\n      </description>\n"
    "    </release>\n"
)
text = path.read_text()
marker = "  <releases>\n"
assert marker in text, "no <releases> element"
path.write_text(text.replace(marker, marker + entry, 1))
PY
fi

cargo update --workspace --quiet
scripts/check-versions.sh >/dev/null
echo "version set to $version (build number $code)"
