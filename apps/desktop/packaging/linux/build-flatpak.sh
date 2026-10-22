#!/usr/bin/env bash
# Builds the Flatpak from source with flatpak-builder (the org.flatpak.Builder app, so nothing
# but Flatpak is needed on the host). Build state goes to TEAMCRAFT_FLATPAK_DIR (default
# ~/.cache/teamcraft-flatpak; several GB). Nothing is installed or published.
#
#   build-flatpak.sh                  build
#   build-flatpak.sh --bundle         build and write teamcraft-desktop.flatpak into the state dir
#   build-flatpak.sh --run [args]     build, then run the app in its sandbox from the build dir
#
# The web UI is built first by build-ui.sh when crates/teamcraft-ui/dist is missing (or with
# TEAMCRAFT_REBUILD_UI=1).
#
# First use installs the build tools into the user's Flatpak installation:
#   flatpak install --user flathub org.flatpak.Builder org.gnome.Sdk//51 org.gnome.Platform//51 \
#     org.freedesktop.Sdk.Extension.rust-stable//26.08 org.freedesktop.Sdk.Extension.llvm22//26.08
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MANIFEST="${HERE}/de.zeitgleich.teamcraft.yml"
STATE="${TEAMCRAFT_FLATPAK_DIR:-${HOME}/.cache/teamcraft-flatpak}"
BUILDER=(flatpak run --filesystem="${STATE}" org.flatpak.Builder)
mkdir -p "${STATE}"
REPO="$(cd "${HERE}/../../../.." && pwd)"
if [ ! -f "${REPO}/crates/teamcraft-ui/dist/index.html" ] || [ "${TEAMCRAFT_REBUILD_UI:-0}" = 1 ]; then
  "${HERE}/build-ui.sh"
fi

"${BUILDER[@]}" --user --force-clean --state-dir="${STATE}/state" --repo="${STATE}/repo" \
  "${STATE}/build" "${MANIFEST}"

case "${1:-}" in
  --bundle)
    flatpak build-bundle "${STATE}/repo" "${STATE}/teamcraft-desktop.flatpak" de.zeitgleich.teamcraft
    echo "bundle: ${STATE}/teamcraft-desktop.flatpak"
    ;;
  --run)
    shift
    "${BUILDER[@]}" --run "${STATE}/build" "${MANIFEST}" teamcraft-desktop "$@"
    ;;
esac
