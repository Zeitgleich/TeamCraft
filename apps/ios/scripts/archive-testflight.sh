#!/usr/bin/env bash
# Archives the app for the App Store, exports the .ipa and uploads it to App Store Connect
# (TestFlight). macOS only. Used by .github/workflows/ios-release.yml; runs the same on a Mac
# (docs/release.md, "TestFlight").
#
# Needs, before it runs:
#   - the Rust core for the device: scripts/build-xcframework.sh --device-only
#   - the "Apple Distribution" certificate with its private key in a keychain on the search list
#   - the three App Store provisioning profiles installed (~/Library/MobileDevice/Provisioning Profiles)
#
# Environment (no defaults: a missing value stops the script):
#   APPLE_TEAM_ID                  the team that signs (10 characters)
#   TC_PROFILE_APP                 name of the App Store profile of de.zeitgleich.teamcraft
#   TC_PROFILE_NOTIFICATIONS       ... of de.zeitgleich.teamcraft.notifications
#   TC_PROFILE_SHARE               ... of de.zeitgleich.teamcraft.share
#   ASC_KEY_ID, ASC_ISSUER_ID      the App Store Connect API key (for the upload)
#   ASC_KEY_PATH                   its .p8 file
# Optional:
#   TEAMCRAFT_PUSH_GATEWAY_URL     the publisher's push gateway built into the app (docs/push-gateway.md §3a)
#
# Usage: scripts/archive-testflight.sh [--no-upload]
#   Always exports the signed .ipa into build/export; uploads it unless --no-upload is given
#   --no-upload  only the .ipa (check signing locally)
set -euo pipefail

UPLOAD=1
for argument in "$@"; do
  case "${argument}" in
    --no-upload) UPLOAD=0 ;;
    *) echo "unknown argument: ${argument}" >&2; exit 2 ;;
  esac
done

for name in APPLE_TEAM_ID TC_PROFILE_APP TC_PROFILE_NOTIFICATIONS TC_PROFILE_SHARE; do
  if [ -z "${!name:-}" ]; then echo "error: ${name} is not set" >&2; exit 1; fi
done
if [ "${UPLOAD}" = 1 ]; then
  for name in ASC_KEY_ID ASC_ISSUER_ID ASC_KEY_PATH; do
    if [ -z "${!name:-}" ]; then echo "error: ${name} is not set (or use --no-upload)" >&2; exit 1; fi
  done
fi

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_DIR="${APP_DIR}/build"
ARCHIVE="${BUILD_DIR}/TeamCraft.xcarchive"
EXPORT_OPTIONS="${BUILD_DIR}/ExportOptions.plist"
cd "${APP_DIR}"

if [ ! -d Generated/TeamCraftFFI.xcframework ]; then
  echo "error: no Rust core; run scripts/build-xcframework.sh --device-only first" >&2
  exit 1
fi
xcodegen generate
rm -rf "${ARCHIVE}" "${BUILD_DIR}/export" "${BUILD_DIR}/upload"
mkdir -p "${BUILD_DIR}"

# Manual signing with the distribution identity: the profiles name the bundle IDs and the
# capabilities, so a missing capability fails here and not at Apple's validation. The App Store
# profile carries `aps-environment = production`; the entitlement must say the same
# (Config/Base.xcconfig keeps `development` for builds signed for development).
xcodebuild archive \
  -project TeamCraft.xcodeproj -scheme TeamCraft -configuration Release \
  -destination "generic/platform=iOS" -archivePath "${ARCHIVE}" \
  DEVELOPMENT_TEAM="${APPLE_TEAM_ID}" \
  CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY="Apple Distribution" \
  APS_ENVIRONMENT=production \
  TC_PROFILE_APP="${TC_PROFILE_APP}" \
  TC_PROFILE_NOTIFICATIONS="${TC_PROFILE_NOTIFICATIONS}" \
  TC_PROFILE_SHARE="${TC_PROFILE_SHARE}" \
  TEAMCRAFT_PUSH_GATEWAY_URL="${TEAMCRAFT_PUSH_GATEWAY_URL:-}" \
  | tail -n 200

# The build number is the release's (docs/release.md, "Build numbers"): App Store Connect must not
# change it. Symbols go to Apple too, so TestFlight crash logs are symbolized.
write_export_options() {
  cat > "$1" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key>
  <string>app-store-connect</string>
  <key>destination</key>
  <string>$2</string>
  <key>teamID</key>
  <string>${APPLE_TEAM_ID}</string>
  <key>signingStyle</key>
  <string>manual</string>
  <key>signingCertificate</key>
  <string>Apple Distribution</string>
  <key>provisioningProfiles</key>
  <dict>
    <key>de.zeitgleich.teamcraft</key>
    <string>${TC_PROFILE_APP}</string>
    <key>de.zeitgleich.teamcraft.notifications</key>
    <string>${TC_PROFILE_NOTIFICATIONS}</string>
    <key>de.zeitgleich.teamcraft.share</key>
    <string>${TC_PROFILE_SHARE}</string>
  </dict>
  <key>manageAppVersionAndBuildNumber</key>
  <false/>
  <key>uploadSymbols</key>
  <true/>
  <key>testFlightInternalTestingOnly</key>
  <false/>
</dict>
</plist>
PLIST
  plutil -lint "$1"
}

# The .ipa first (it is attached to the GitHub release), then the upload from the same archive.
write_export_options "${EXPORT_OPTIONS}" export
xcodebuild -exportArchive -archivePath "${ARCHIVE}" -exportOptionsPlist "${EXPORT_OPTIONS}" \
  -exportPath "${BUILD_DIR}/export" | tail -n 100
echo "Exported to ${BUILD_DIR}/export."

if [ "${UPLOAD}" = 1 ]; then
  write_export_options "${BUILD_DIR}/ExportOptions-upload.plist" upload
  xcodebuild -exportArchive -archivePath "${ARCHIVE}" \
    -exportOptionsPlist "${BUILD_DIR}/ExportOptions-upload.plist" -exportPath "${BUILD_DIR}/upload" \
    -authenticationKeyPath "${ASC_KEY_PATH}" -authenticationKeyID "${ASC_KEY_ID}" \
    -authenticationKeyIssuerID "${ASC_ISSUER_ID}" | tail -n 100
  echo "Uploaded to App Store Connect; TestFlight lists the build once Apple has processed it."
fi
