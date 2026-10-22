# Releases

How a TeamCraft release is versioned, built and published. Operators: what a release contains and
how to install or upgrade it is in [operations.md](operations.md).

## Versions

One version for everything: the Rust workspace (`[workspace.package] version` in `Cargo.toml`, which
every crate inherits), the desktop app (`apps/desktop/tauri.conf.json`), the Android app
(`versionName` in `apps/android/app/build.gradle.kts`) and, once it is merged, the iOS app. Versions
follow [Semantic Versioning](https://semver.org); prereleases are `-beta.N` and `-rc.N`.

| Version        | Meaning                                                                       |
|----------------|-------------------------------------------------------------------------------|
| `1.0.0-beta.N` | Feature complete for v1 (PLAN.md), in use by testers; schema may still change |
| `1.0.0-rc.N`   | Candidate; only fixes                                                         |
| `1.0.0`        | Release; from here on the wire format and the API stay compatible within 1.x  |

Database migrations are forward-only (`.claude/rules/api-conventions.md`), also between prereleases.

### Android `versionCode`

Google Play needs a strictly increasing integer:
`MAJOR * 1000000 + MINOR * 10000 + PATCH * 100 + stage`, where stage is `N` for `beta.N` (1-49),
`50 + N` for `rc.N` (1-48) and `99` for the release.

| Version        | `versionCode` |
|----------------|---------------|
| `1.0.0-beta.1` | `1000001`     |
| `1.0.0-rc.2`   | `1000052`     |
| `1.0.0`        | `1000099`     |
| `1.0.1`        | `1000199`     |
| `1.1.0-beta.1` | `1010001`     |

### Packaging limits of prerelease versions

- **rpm** has no `-` in versions; the bundler writes `1.0.0-beta.1` as is, so the file is named
  `TeamCraft-1.0.0-beta.1-1.x86_64.rpm` and `rpm -q` shows it that way. It installs and upgrades to
  `1.0.0`, but tools that split the file name (not the header) get it wrong. Fine for betas.
- **Windows MSI** only allows a numeric prerelease (`1.0.0-1`); the Windows build uses NSIS, which has
  no such limit. If an MSI is added, set `bundle.windows.wix.version` for prereleases.

## Release procedure

1. **Main is green.** Never tag a commit whose CI failed.
2. **Branch** `release/vX.Y.Z` (or do it on the feature branch that finishes the release) and set the
   version in the three places above. `cargo update --workspace --offline` updates `Cargo.lock`. Set the
   Android `versionCode` from the table. Update `<releases>` in
   `apps/desktop/packaging/linux/org.teamcraft.desktop.metainfo.xml`.
3. **Changelog.** Move the entries under `## [Unreleased]` in `CHANGELOG.md` to
   `## [X.Y.Z] - YYYY-MM-DD` and update the compare links at the bottom. The section becomes the
   release notes.
4. **Merge** (squash) into main and wait for CI.
5. **Tag** the merge commit and push the tag:

   ```sh
   git tag -s vX.Y.Z -m "TeamCraft X.Y.Z"
   git push origin vX.Y.Z
   ```

6. **Watch** the `Release` workflow (`.github/workflows/release.yml`). Its first job stops when the tag
   does not match the versions in the tree or `CHANGELOG.md` has no section for it.
7. **Review the draft release** it creates: the file list (below), the notes, `images.txt`. Install
   at least the server images on a staging host (`operations.md`, "Upgrades") and one desktop
   package.
8. **Publish** the draft in the GitHub UI. Prereleases are marked as such automatically.
9. **Deploy**: put the image digests from `images.txt` into the deployment's inventory.

A failed run can be re-run from the Actions UI; the last job uploads into the existing draft with
`--clobber`. Never move a published tag. If a release is broken, publish a new patch version.

## What a release contains

| File                                                | Built by                                     |
|-----------------------------------------------------|----------------------------------------------|
| `ghcr.io/zeitgleich/teamcraft-server:X.Y.Z`         | `image` jobs (amd64 + arm64, native runners) |
| `ghcr.io/zeitgleich/teamcraft-sfu:X.Y.Z`            | `image` jobs                                 |
| `images.txt`                                        | image references with their digests          |
| `teamcraft-server-X.Y.Z.spdx.json`, `-sfu-...`      | SPDX SBOMs of the images (syft)              |
| `teamcraft-source-X.Y.Z.spdx.json`                  | SPDX SBOM of the source tree (Cargo, Gradle) |
| `TeamCraft_X.Y.Z_amd64.deb`, `.rpm`, `.AppImage`    | `desktop-release.yml`, x86_64 and aarch64    |
| `teamcraft-desktop-<arch>.flatpak`                  | `desktop-release.yml`                        |
| `TeamCraft_X.Y.Z_<arch>.dmg`                        | `desktop-release.yml`, unsigned              |
| `teamcraft-web-X.Y.Z.tar.gz`                        | the web client (`[web] static_dir`)          |
| `teamcraft-android-X.Y.Z-{full,libre}-release*.apk` | `android` job; see signing below             |
| `teamcraft-android-X.Y.Z-{full,libre}-debug.apk`    | `android` job; debug-signed, for testers     |
| `SHA256SUMS`                                        | over every file above                        |

Images are tagged with the full version and, for releases, `X.Y`; there is no `latest`. The image
builds also push SBOM and provenance attestations to the registry
(`docker buildx imagetools inspect --format '{{json .SBOM}}' <image>`). Images are pushed with the
workflow's `GITHUB_TOKEN`; the packages inherit the repository's visibility on first push, change it
under the organization's *Packages* settings if they should be public.

### Signing

| Artifact | Status                                                                                   |
|----------|------------------------------------------------------------------------------------------|
| Android  | Release APKs signed with the upload key when its secrets exist (below), else `-unsigned` |
| macOS    | Unsigned, not notarized; the secrets it would need are listed in `desktop-release.yml`.  |
| Linux    | Packages are unsigned; verify them with `SHA256SUMS`.                                    |
| Images   | Not signed yet; pin them by digest from `images.txt`.                                    |

Upload key secrets: `ANDROID_UPLOAD_KEYSTORE_BASE64` (the keystore file, base64),
`ANDROID_UPLOAD_KEYSTORE_PASSWORD`, `ANDROID_UPLOAD_KEY_ALIAS`, `ANDROID_UPLOAD_KEY_PASSWORD`.

The Android apps read their server and identity provider defaults at build time. The release job takes
them from the repository variables `TEAMCRAFT_ANDROID_SERVER_URL`, `TEAMCRAFT_ANDROID_OIDC_ISSUER`,
`TEAMCRAFT_ANDROID_OIDC_CLIENT_ID` and `TEAMCRAFT_ANDROID_OIDC_REDIRECT_SCHEME`; without them the APKs
point at the development defaults in `apps/android/gradle.properties`.

## Checking the workflows locally

```sh
docker run --rm -v "$PWD:/repo" -w /repo docker.io/rhysd/actionlint:1.7.7
```

A tag push is the only trigger of `release.yml`; to try the pipeline without a release, push a tag to
a fork. `desktop-release.yml` alone can be run by hand from the Actions UI.
