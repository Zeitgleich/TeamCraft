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

| Tag on a tree with version `1.0.0` | Result                                                                                                        |
|------------------------------------|---------------------------------------------------------------------------------------------------------------|
| `v1.0.0`                           | release: images get `1.0.0`, `1.0` and `latest`                                                               |
| `v1.0.0-rc.1`                      | candidate: the same pipeline as a prerelease; images get `1.0.0-rc.1` only, packages carry the tree's `1.0.0` |
| anything else                      | the first job fails                                                                                           |

A candidate tag tests the whole pipeline on exactly the tree that becomes the release. A tree with a
prerelease version (`1.1.0-beta.1`) is released with exactly that tag.

Prerelease versions in the tree have packaging limits: rpm writes the `-` as is (fine for betas), and
Windows MSI only allows numeric prereleases (set `bundle.windows.wix.version` then; NSIS has no limit).

## Release procedure

1. **Main is green.** Never tag a commit whose CI failed.
2. **Branch** `release/vX.Y.Z` and set the version in the places above (`cargo update --workspace
   --offline` updates `Cargo.lock`); `scripts/check-versions.sh` must pass.
3. **Changelog.** Move the entries under `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` and update the
   compare links. Put changes that need operators to act under `### Breaking`: the release notes show
   them under "Breaking changes". The section is the notes' "Highlights".
4. **Merge** (squash) into main and wait for CI.
5. **Candidate**: tag the merge commit `vX.Y.Z-rc.1` and push the tag. Check the draft prerelease (files,
   notes, `images.txt`), install the images on a staging host and one desktop package, then delete the
   draft (the candidate's images stay in GHCR under their own tag).
6. **Release**: tag the same commit and push it:

   ```sh
   git tag -s vX.Y.Z -m "TeamCraft X.Y.Z"
   git push origin vX.Y.Z
   ```

7. **Review the draft release**, then **publish** it in the GitHub UI.
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
| `TeamCraft_X.Y.Z_universal.dmg`                                 | `desktop-release.yml`, unsigned                  |
| `TeamCraft_X.Y.Z_{x64,arm64}-setup.exe`, `.msi`                 | `desktop-release.yml`, unsigned                  |
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

### Signing the platform packages

| Artifact | Status                                                                                   |
|----------|------------------------------------------------------------------------------------------|
| Android  | Release APKs signed with the upload key when its secrets exist (below), else `-unsigned` |
| macOS    | Unsigned, not notarized; the secrets it would need are listed in `desktop-release.yml`   |
| Windows  | Not code-signed (`apps/desktop/packaging/windows/README.md`)                             |
| iOS      | Not built: build and sign it with your own Apple team (`apps/ios/README.md`)             |
| Linux    | Packages are unsigned; checksums and attestations cover them                             |

Upload key secrets: `ANDROID_UPLOAD_KEYSTORE_BASE64` (the keystore file, base64),
`ANDROID_UPLOAD_KEYSTORE_PASSWORD`, `ANDROID_UPLOAD_KEY_ALIAS`, `ANDROID_UPLOAD_KEY_PASSWORD`.

The Android apps read their server and identity provider defaults at build time. The release job takes
them from the repository variables `TEAMCRAFT_ANDROID_SERVER_URL`, `TEAMCRAFT_ANDROID_OIDC_ISSUER`,
`TEAMCRAFT_ANDROID_OIDC_CLIENT_ID` and `TEAMCRAFT_ANDROID_OIDC_REDIRECT_SCHEME`; without them the APKs
point at the development defaults in `apps/android/gradle.properties`.

## Checking locally

```sh
scripts/check-versions.sh v1.0.0-rc.1        # the tag fits the tree
docker run --rm -v "$PWD:/repo" -w /repo docker.io/rhysd/actionlint:1.7.12
echo "ghcr.io/x/teamcraft-server:1.0.0@sha256:0" > /tmp/images.txt
scripts/render-release-notes.py --version 1.0.0 --tag v1.0.0 --repo Zeitgleich/TeamCraft --images /tmp/images.txt
```

A tag push is the only trigger of `release.yml`; to try it without a release, push a candidate tag
(above) or a tag to a fork. `desktop-release.yml` alone can be run by hand from the Actions UI.
