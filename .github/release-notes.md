<!-- Template of every GitHub release's notes, rendered by scripts/render-release-notes.py
     (docs/release.md). Placeholders: {{version}} {{tag}} {{repo}} {{prerelease_notice}}
     {{changelog}} {{breaking}} {{images}} {{generated_notes}} -->
{{prerelease_notice}}
## Highlights

{{changelog}}

## Install

**Server** – with the deployment playbook [teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy)
(`teamcraft_version: "{{version}}"`), or run the images yourself (`docs/operations.md`). Pin them by
digest:

```
{{images}}
```

**Desktop**

| Platform                        | File                                                            |
|---------------------------------|-----------------------------------------------------------------|
| Windows x64 / arm64             | `TeamCraft_*_x64-setup.exe` / `*_arm64-setup.exe` (or `.msi`)   |
| macOS (Apple silicon and Intel) | `TeamCraft_*_universal.dmg`                                     |
| Linux x86_64 / aarch64          | `.deb`, `.rpm`, `.AppImage`, `teamcraft-desktop-<arch>.flatpak` |

**Android** – `teamcraft-android-{{version}}-full-release*.apk` (with Firebase push) or
`-libre-release*.apk` (without Google services). Files ending in `-unsigned.apk` must be signed before
they install; the `-debug-signed.apk` files install as they are and are meant for testing only.

**iOS** – no binary: build it from `apps/ios` with your own Apple team and signing (`apps/ios/README.md`).

**Web client** – `teamcraft-web-{{version}}.tar.gz`, served by the server (`[web] static_dir`;
`teamcraft_server_web_enabled` in the playbook).

## Upgrading

Back up the database first. Database migrations run by themselves when the new server starts and are
forward-only: going back means restoring the backup. Update the server before the SFU; clients of the
previous release keep working.

### Breaking changes

{{breaking}}

## Known limitations

- **Unsigned desktop builds**: the macOS dmg is neither signed nor notarized (open it with right-click
  → Open, or `xattr -d com.apple.quarantine`), the Windows installers are not code-signed (SmartScreen
  asks once). Verify them with the checksums and attestations below.
- **Android**: release APKs are only signed when the project's upload key is configured for the build;
  otherwise use the `-debug-signed` APKs for testing or sign the release APKs yourself. There is no
  Play Store or F-Droid listing yet.
- **iOS is not distributed**: there is no App Store or TestFlight build. Build it from `apps/ios` with
  your own Apple team and signing; push then goes through your own APNs key (`[push.apns]`), not the
  push gateway.
- **Push for self-built mobile apps** needs the server's own APNs / FCM credentials; the publisher's
  push gateway only serves the official store apps.
- **Tested in CI only, not yet on real hardware**: the macOS and Windows desktop apps (built, unit-tested
  and packaged in CI; hardware video encoding through VideoToolbox / Media Foundation falls back to
  software when unavailable), the iOS app (compiled and unit-tested in CI, not yet run on a device) and
  Android video calls (audio calls are tested). Linux desktop, web, Android chat and the server are
  covered by the end-to-end and acceptance suites.
- The AppImage is large (~130 MB): it carries WebKitGTK and its dependencies; the deb, rpm and Flatpak
  use the system's.
- Calls in the browser need WebTransport (Chromium-based browsers, Firefox).

## Verify the downloads

```sh
sha256sum -c SHA256SUMS --ignore-missing                       # the files you downloaded
gh attestation verify <file> --repo {{repo}}                   # built by this repository's release workflow
cosign verify ghcr.io/zeitgleich/teamcraft-server:{{version}} \
  --certificate-identity-regexp '^https://github.com/{{repo}}/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
gh attestation verify oci://ghcr.io/zeitgleich/teamcraft-server:{{version}} --repo {{repo}}
```

SBOMs: `teamcraft-source-{{version}}.{spdx,cdx}.json` for the source tree, `teamcraft-server-*` and
`teamcraft-sfu-*` for the images.

## What's changed

{{generated_notes}}
