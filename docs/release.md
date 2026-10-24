# Releases

How a TeamCraft release is versioned, built and published. Operators: installing and upgrading is in
[operations.md](operations.md) and the deployment playbook
[teamcraft-deploy](https://github.com/Zeitgleich/teamcraft-deploy).

## Versions

One version for everything, checked by `scripts/check-versions.sh` (CI job `versions`, and the
release workflow against the tag):

| Where                                         | Field                                                                           |
|-----------------------------------------------|---------------------------------------------------------------------------------|
| `Cargo.toml`                                  | `[workspace.package] version` (every crate)                                     |
| `apps/desktop/tauri.conf.json`                | `version`                                                                       |
| `apps/android/app/build.gradle.kts`           | `versionName`, `versionCode` (formula below)                                    |
| `apps/ios/project.yml`                        | `MARKETING_VERSION` (numeric part), `CURRENT_PROJECT_VERSION` (= `versionCode`) |
| `CHANGELOG.md`                                | a `## [X.Y.Z]` section                                                          |
| `apps/desktop/packaging/linux/*.metainfo.xml` | `<release>` (not checked; update it)                                            |

Versions follow [Semantic Versioning](https://semver.org); prereleases in the tree are `-beta.N` and
`-rc.N`. From 1.0.0 on the wire format, the API and the database stay compatible within 1.x; database
migrations are forward-only (`.claude/rules/api-conventions.md`).

### Build numbers

Google Play and the App Store need strictly increasing integers:
`MAJOR * 1000000 + MINOR * 10000 + PATCH * 100 + stage`, where stage is `N` for `beta.N` (1-49),
`50 + N` for `rc.N` (1-48) and `99` for the release. Android's `versionCode` and iOS's
`CURRENT_PROJECT_VERSION` both use it; iOS's `MARKETING_VERSION` is the numeric part only.

| Version        | Build number |
|----------------|--------------|
| `1.0.0-rc.2`   | `1000052`    |
| `1.0.0`        | `1000099`    |
| `1.0.1`        | `1000199`    |
| `1.1.0-beta.1` | `1010001`    |

### Tags

| Tag on a tree with version `1.0.0` | Result                                                                                                           |
|------------------------------------|------------------------------------------------------------------------------------------------------------------|
| `v1.0.0`                           | release: images get `1.0.0`, `1.0` and `latest`                                                                  |
| `v1.0.0-rc.1`                      | candidate: the same pipeline as a prerelease; images get `1.0.0-rc.1` only, deb `1.0.0~rc.1`, rpm `1.0.0-0.rc.1` |
| anything else                      | the first job fails                                                                                              |

A candidate tag tests the whole pipeline on exactly the tree that becomes the release. A tree with a
prerelease version (`1.1.0-beta.1`) is released with exactly that tag.

Prerelease versions in the tree have packaging limits: Windows MSI only allows numeric prereleases
(set `bundle.windows.wix.version` then; NSIS has no limit).

### Linux package versions

Tauri names the deb and rpm after the tree's version, so a candidate would carry the release's
version and `dnf`/`apt` would never upgrade between candidates or from a candidate to the release.
`desktop-release.yml` therefore maps the tag with `scripts/linux-package-version.sh`:

| Tag           | deb          | rpm (version-release) | Update manifest (`latest.json`) |
|---------------|--------------|-----------------------|---------------------------------|
| `v1.0.0-rc.2` | `1.0.0~rc.2` | `1.0.0-0.rc.2`        | `1.0.0-rc.2`                    |
| `v1.0.0`      | `1.0.0`      | `1.0.0-1`             | `1.0.0`                         |

The rpm gets `bundle.linux.rpm.release` through a `--config` override (Tauri 2 has no per-package
version override, and the config version must stay semver); the deb is repacked with the new
`Version` after the build. The `Version` job of `release.yml` checks with `rpm` and
`dpkg --compare-versions` that the candidate sorts before the release (and `rc.2` before `rc.10`).
Candidates are only published in the `prerelease` repositories; `stable` gets the release.

Candidate installs of 1.0.0 (`1.0.0-1` rpm / `1.0.0` deb, built before this mapping) have the same
version as the final 1.0.0 and as each other: reinstall once with `sudo dnf reinstall team-craft` or
`sudo apt install --reinstall team-craft`; later releases upgrade normally.

## Release procedure

1. **Main is green.** Never tag a commit whose CI failed.
2. **Branch** `release/vX.Y.Z` and set the version in the places above (`cargo update --workspace
   --offline` updates `Cargo.lock`); `scripts/check-versions.sh` must pass.
3. **Changelog.** Move the entries under `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` and update the
   compare links. Put changes that need operators to act under `### Breaking`: the release notes show
   them under "Breaking changes". The section is the notes' "Highlights".
4. **Merge** (squash) into main and wait for CI.
5. **Candidate**: tag the merge commit `vX.Y.Z-rc.1` and push the tag. Check the draft prerelease (files,
   notes, `images.txt`, `latest.json`), install the images on a staging host and one desktop package,
   then delete the draft (the candidate's images stay in GHCR under their own tag) – or publish it as
   a prerelease, which hands it to the `prerelease` update channel and repositories
   ([updates.md](updates.md) §3.2).
   Note: tag a commit whose message has no `[skip ci]`; GitHub skips every workflow for a tag on
   such a commit, the release included.
6. **Release**: tag the same commit and push it:

   ```sh
   git tag -s vX.Y.Z -m "TeamCraft X.Y.Z"
   git push origin vX.Y.Z
   ```

7. **Review the draft release**, then **publish** it in the GitHub UI. Publishing runs `packages.yml`:
   the Pages site with the update manifests and the APT/RPM repositories is rebuilt, and installed
   desktop apps see the release (stable: `releases/latest/download/latest.json`).
8. **Deploy**: release teamcraft-deploy with `teamcraft_version: "X.Y.Z"` (its own CHANGELOG and tag).

A failed run can be re-run from the Actions UI; the last job uploads into the existing draft with
`--clobber`. Never move a published tag. If a release is broken, publish a new patch version.

## Release notes

Rendered by `scripts/render-release-notes.py` from the template `.github/release-notes.md`:

| Part                        | Comes from                                                                                                             |
|-----------------------------|------------------------------------------------------------------------------------------------------------------------|
| Pre-release notice          | tags with a `-` suffix                                                                                                 |
| Highlights                  | the version's `CHANGELOG.md` section (for a candidate: the base version's)                                             |
| Install                     | the template, with the image references of `images.txt`                                                                |
| Upgrading, breaking changes | the template; `### Breaking` of the changelog section, else "None."                                                    |
| Known limitations           | the template: unsigned builds, iOS not distributed, push for self-built apps, what is tested in CI only, AppImage size |
| Verify the downloads        | the template: checksums, attestations, cosign                                                                          |
| What's changed              | GitHub's generated notes, grouped by `.github/release.yml`                                                             |

Label pull requests so the generated notes group them: `security`, `breaking`, `feature` /
`enhancement`, `bug` / `fix`, `documentation`, `dependencies`; `skip-changelog` leaves one out.
Dependabot's pull requests are left out. Edit the template when a known limitation is gone.

## What a release contains

| File                                                            | Built by                                         |
|-----------------------------------------------------------------|--------------------------------------------------|
| `ghcr.io/zeitgleich/teamcraft-server:X.Y.Z` (+ `X.Y`, `latest`) | `image` jobs (amd64 + arm64, native runners)     |
| `ghcr.io/zeitgleich/teamcraft-sfu:X.Y.Z` (+ `X.Y`, `latest`)    | `image` jobs                                     |
| `images.txt`                                                    | image references with their digests              |
| `teamcraft-{server,sfu}-X.Y.Z.{spdx,cdx}.json`                  | SBOMs of the images (syft; SPDX, CycloneDX)      |
| `teamcraft-source-X.Y.Z.{spdx,cdx}.json`                        | SBOMs of the source tree (Cargo, Gradle)         |
| `TeamCraft_X.Y.Z_*.deb`, `.rpm`, `.AppImage`                    | `desktop-release.yml`, x86_64 and aarch64        |
| `teamcraft-desktop-<arch>.flatpak`                              | `desktop-release.yml`                            |
| `TeamCraft_X.Y.Z_{aarch64,x64}.dmg`                             | `desktop-release.yml`, Apple silicon and Intel   |
| `TeamCraft_X.Y.Z_{x64,arm64}-setup.exe`, `.msi`                 | `desktop-release.yml`, unsigned                  |
| `TeamCraft_X.Y.Z_{aarch64,x64}.app.tar.gz`                      | `desktop-release.yml`, the macOS update archives |
| `*.sig` next to the AppImages, installers and `.app.tar.gz`     | updater signatures (with the updater key only)   |
| `latest.json`                                                   | updater manifest (`scripts/updater-manifest.py`) |
| `teamcraft-web-X.Y.Z.tar.gz`                                    | the web client (`[web] static_dir`)              |
| `teamcraft-android-X.Y.Z-{full,libre}-release.apk`              | `android` job, signed with the upload key        |
| `teamcraft-android-X.Y.Z-{full,libre}-release-unsigned.apk`     | instead, when no upload key is configured        |
| `teamcraft-android-X.Y.Z-{full,libre}-debug-signed.apk`         | `android` job; throwaway debug key, testing only |
| `SHA256SUMS`                                                    | over every file above                            |

## Integrity: checksums, attestations, signatures

- **Checksums**: `sha256sum -c SHA256SUMS --ignore-missing`.
- **Build provenance** (SLSA, Sigstore): every file in `SHA256SUMS` and both images have an
  attestation from `actions/attest-build-provenance`, tied to this repository's release workflow and
  the tag: `gh attestation verify <file> --repo Zeitgleich/TeamCraft`, and
  `gh attestation verify oci://ghcr.io/zeitgleich/teamcraft-server:X.Y.Z --repo Zeitgleich/TeamCraft`.
- **Image signatures**: cosign, keyless (the certificate names the workflow and the tag):

  ```sh
  cosign verify ghcr.io/zeitgleich/teamcraft-server:X.Y.Z \
    --certificate-identity-regexp '^https://github.com/Zeitgleich/TeamCraft/\.github/workflows/release\.yml@refs/tags/v' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com
  ```

- The image builds also push buildx SBOM and provenance attestations to the registry
  (`docker buildx imagetools inspect --format '{{json .SBOM}}' <image>`).

Images are pushed with the workflow's `GITHUB_TOKEN`; the packages inherit the repository's visibility
on first push, change it under the organization's *Packages* settings if they should be public.

### The macOS app's signature

The release job builds the app, signs it inside out (`apps/desktop/packaging/macos/sign-app.sh`:
every library and plugin of the bundled GStreamer framework, the framework, then the app with
`Entitlements.plist`) and only then packs the dmg (`make-dmg.sh`). It fails when the signature is
broken (`verify-app.sh`: `codesign --verify --deep --strict` and `spctl`), on the app and again on the
app inside the mounted dmg. A broken or missing seal is what Gatekeeper calls "damaged"; an intact
ad-hoc signature gets the normal "could not verify" prompt instead.

With the secrets `APPLE_CERTIFICATE` (the Developer ID Application certificate as base64 .p12),
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` (an app-specific
password) and `APPLE_TEAM_ID`, the same job signs with the Developer ID, with hardened runtime and
timestamp, notarizes and staples the app and the dmg, and requires Gatekeeper to accept them.

### Opening the unsigned macOS app

1. Drag TeamCraft from the dmg to *Applications* and open it once; macOS refuses ("Apple could not
   verify …").
2. *System Settings → Privacy & Security*, section *Security*: *Open Anyway*, confirm with the password.
   On macOS 14, right-click → *Open* in Finder also works.
3. Only if that doesn't appear: `xattr -dr com.apple.quarantine /Applications/TeamCraft.app`.

"TeamCraft is damaged and can't be opened" means a broken signature: report it, don't work around it.

### Signing the platform packages

| Artifact | Status                                                                                     |
|----------|--------------------------------------------------------------------------------------------|
| Android  | Release APKs signed with the upload key when its secrets exist (below), else `-unsigned`   |
| macOS    | Ad-hoc signed, not notarized; Developer ID signing and notarization with the secrets above |
| Windows  | Not code-signed (`apps/desktop/packaging/windows/README.md`)                               |
| iOS      | Not built: build and sign it with your own Apple team (`apps/ios/README.md`)               |
| Linux    | Packages are unsigned; checksums and attestations cover them; the repositories sign them   |
| Updates  | minisign signatures for the updater when `TAURI_SIGNING_PRIVATE_KEY` exists (below)        |
| Web      | Signed release manifest (`release.json`) when `TEAMCRAFT_WEB_RELEASE_KEY` exists (below)   |

Updater and repository secrets (how to create them: [updates.md](updates.md) §7):
`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (the public key goes into
`plugins.updater.pubkey` of `apps/desktop/tauri.conf.json`), `LINUX_REPO_GPG_PRIVATE_KEY`,
`LINUX_REPO_GPG_PASSPHRASE`. Without the updater key a release has no `latest.json` and installed apps
don't see it; without the repository key the Pages site has the manifests but no APT/RPM repositories.

Web client key (e2ee.md §9.3): `TEAMCRAFT_WEB_RELEASE_KEY`, an Ed25519 private key in PEM. The release
signs the web bundle with it (`crates/teamcraft-ui/scripts/sign-web-release.sh`: `release.json` plus the
public key pinned into `sw.js`); without it the web client is unsigned and shows a warning banner.
Keep it stable: the About screen shows its fingerprint, so users and auditors can compare it between
releases. Create it once:

```sh
openssl genpkey -algorithm ed25519 -out teamcraft-web-release.pem
gh secret set TEAMCRAFT_WEB_RELEASE_KEY < teamcraft-web-release.pem
```

Every release checks the web bundle for demo data first (`scripts/check-no-demo-data.py`); the desktop
jobs check the bundle they embed.

Upload key secrets: `ANDROID_UPLOAD_KEYSTORE_BASE64` (the keystore file, base64),
`ANDROID_UPLOAD_KEYSTORE_PASSWORD`, `ANDROID_UPLOAD_KEY_ALIAS`, `ANDROID_UPLOAD_KEY_PASSWORD`.

No release build has a server built in: the desktop, Android and iOS apps ask the user for the server
address on first start (or take it from the device's managed configuration) and learn how to sign in
from the server's `GET /api/v1/server-info` (docs/operations.md "Server address in the apps"). The
release job therefore sets no server or identity provider; the redirect scheme stays the app ID
(`de.zeitgleich.teamcraft:/oauth/callback`), the one operators register. Self-builds may compile in a
default server (`TEAMCRAFT_DEFAULT_SERVER_URL` for desktop and iOS, `teamcraft.defaultServer` for
Android); release builds must not.

The `full` flavor gets its Firebase project from the secrets `TEAMCRAFT_FCM_PROJECT_ID`,
`TEAMCRAFT_FCM_APPLICATION_ID`, `TEAMCRAFT_FCM_API_KEY` and `TEAMCRAFT_FCM_SENDER_ID` (the values of the
app's `google-services.json`; `teamcraft.fcm*` in `apps/android/README.md`). A release tag fails without
them, a candidate tag builds without FCM and warns. Store builds seal their FCM token for the push
gateway in the repository variable `TEAMCRAFT_ANDROID_PUSH_GATEWAY_URL` (e.g.
`https://push-gw.zeitgleich.de`); without it they use the gateway the server names.

## Checking locally

```sh
scripts/check-versions.sh v1.0.0-rc.1        # the tag fits the tree
docker run --rm -v "$PWD:/repo" -w /repo docker.io/rhysd/actionlint:1.7.12
echo "ghcr.io/x/teamcraft-server:1.0.0@sha256:0" > /tmp/images.txt
scripts/render-release-notes.py --version 1.0.0 --tag v1.0.0 --repo Zeitgleich/TeamCraft --images /tmp/images.txt
```

A tag push is the only trigger of `release.yml`; to try it without a release, push a candidate tag
(above) or a tag to a fork. `desktop-release.yml` alone can be run by hand from the Actions UI.
