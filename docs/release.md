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

Releases are made by [the nightly workflow](#nightly-releases); this is the manual procedure for
candidates, prereleases and special cases.

1. **Main is green.** Never tag a commit whose CI failed.
2. **Branch** `release/vX.Y.Z` and set the version in the places above (`cargo update --workspace
   --offline` updates `Cargo.lock`); `scripts/check-versions.sh` must pass.
3. **Highlights.** There is no changelog file: the release notes live in GitHub Releases. Write the
   version's highlights as Markdown in a file you don't commit; they become the annotated tag's
   message in steps 5 and 6. Put changes that need operators to act under `### Breaking`: the release
   notes show them under "Breaking changes" (else "None.").
4. **Merge** (squash) into main and wait for CI.
5. **Candidate**: tag the merge commit `vX.Y.Z-rc.1` (annotated as in step 6; a candidate with an
   empty message takes the highlights of `vX.Y.Z` if that tag exists) and push the tag. Check the draft prerelease (files,
   notes, `images.txt`, `latest.json`), install the images on a staging host and one desktop package,
   then delete the draft (the candidate's images stay in GHCR under their own tag) – or publish it as
   a prerelease, which hands it to the `prerelease` update channel and repositories
   ([updates.md](updates.md) §3.2).
   Note: tag a commit whose message has no `[skip ci]`; GitHub skips every workflow for a tag on
   such a commit, the release included.
6. **Release**: tag the same commit and push it:

   ```sh
   git tag -s --cleanup=whitespace -F highlights.md vX.Y.Z
   git push origin vX.Y.Z
   ```

   The tagged commit must not contain the skip marker (`[skip ci]`, also inside a PR title) in its
   message: GitHub then skips every workflow
   its push would start, the Release run included. If the last PR landed with `[skip ci]`, land one
   more commit without it and tag that.

   For example, `highlights.md`:

   ```markdown
   - Threads in the sidebar
   - Faster channel switch

   ### Breaking

   - `[push] gateway_url` is now required
   ```

   `--cleanup=whitespace` matters: git's default cleanup drops every line starting with `#`, the
   Markdown headings included. A lightweight tag (no message) is no error: the notes then have only
   the generated part. Check the message with `git tag -l --format='%(contents)' vX.Y.Z`.

7. **Review the draft release**, then **publish** it in the GitHub UI. The draft appears once the
   images, desktop packages and web client are built; the Android files join it when the
   `android-release` job is done (see below). Wait for them before publishing if you can.
   Publishing runs `packages.yml`:
   the Pages site with the update manifests and the APT/RPM repositories is rebuilt, and installed
   desktop apps see the release (stable: `releases/latest/download/latest.json`). It also runs
   `google-play.yml`, which uploads the App Bundle to Google Play's internal testing track
   ([Google Play](#google-play)); a published candidate goes there too.

   GitHub Pages ignores a deployment of a build version it has already deployed, and the candidate
   was deployed from this very commit. The *Publish* job therefore creates the deployment itself
   with a build version unique per run (SHA, run id and attempt) instead of using
   `actions/deploy-pages`, and then waits until the live site serves its build (`site-build.txt`);
   it fails after 10 minutes if it doesn't. This is not yet proven on a real final release. When it
   fails, rebuild the site from the default branch:

   ```sh
   gh workflow run packages.yml --ref main
   ```

   If `main` still points at the release commit, that run is ignored as well; the site then goes live
   with the next run after `main` moves on.
8. **Deploy**: release teamcraft-deploy with `teamcraft_version: "X.Y.Z"` (its own CHANGELOG and tag).

A failed run can be re-run from the Actions UI; the last job uploads into the existing draft with
`--clobber`. Never move a published tag. If a release is broken, publish a new patch version.

Android doesn't hold up the rest (#696): the `release` job (draft release) needs the images and the
desktop workflow only, so the server can be deployed (it reads the web bundle's line in
`SHA256SUMS`) while the Android build runs or is re-run. `android-release` then uploads the APKs and
the App Bundle into the release, draft or published, attests them, and merges their lines into
`SHA256SUMS` (`scripts/merge-checksums.py`; a re-run of either job keeps the other's lines). When the
release was already published by then, the publish event's `google-play.yml` and `packages.yml` ran
without the Android files, so the job starts both again. The SDK install inside the Android build
(`apps/android/scripts/build.sh`) checks every package and retries a corrupt download up to four
times; the SDK isn't cached between runs (it is large, and the cache space is shared with the Rust
jobs).

## Nightly releases

Normal case: pull requests are merged into `main` as soon as they are done, and
`nightly-release.yml` releases `main` every night at 03:00 (Europe/Berlin). The steps of
[Release procedure](#release-procedure) are only done by hand for a candidate or a prerelease.

| Situation                                                       | Result                                                                                |
|-----------------------------------------------------------------|---------------------------------------------------------------------------------------|
| No commit since the last release tag, or only docs / Markdown   | nothing (notice in the run)                                                           |
| CI of `main` is not green for the newest code change           | run fails, nothing tagged; fix `main`, then run the workflow again                    |
| The last tag has no published release                           | run fails: re-run it (`gh workflow run release.yml --ref vX.Y.Z -f publish=true`)    |
| Tree version equals the last tag                                | patch bump commit on `main` (`scripts/bump-version.sh`), tag, release `X.Y.(Z+1)`      |
| A pull request raised the tree's version above the last tag     | the tree is released as it is (minor / major releases: bump in a pull request)         |
| Tree version is a prerelease (`-beta.N`)                        | run fails; release by hand                                                            |

The bump commit (`Release X.Y.Z`, signed off by `github-actions[bot]`) is pushed to `main` directly
and carries no skip marker. `main` requires the `ci-gate` check (repository ruleset; see AGENTS.md,
"Merging"), and GitHub Actions can't be exempted from a repository ruleset, so the run pushes the
commit to a helper branch `nightly/bump-X.Y.Z`, marks its `ci-gate` as passed (the bump only changes
version numbers of a tree whose CI was green), pushes it to `main` and deletes the helper branch. The annotated tag's message is the highlights
(`scripts/nightly-notes.py`): the title of every merged pull request (newest first, at most 15, then
"and N more"; `security` and `breaking` ones first), and the `breaking` ones again under
`### Breaking`. Write pull request titles for readers; `skip-release-notes` leaves one out. Labels
(`feature`, `bug`, ...) group the generated list: that list is "What's changed" ([Release notes](#release-notes)).

Pushes made with `GITHUB_TOKEN` start no workflows, so the nightly run starts `release.yml` itself
with `workflow_dispatch` on the tag (`publish: true`). After the images, desktop packages and web
client are built, its `publish` job publishes the release and starts `packages.yml` (update
manifests, APT/RPM) and `google-play.yml`, which `release: published` would not do for a token
publish. If the Android build failed, the release is published without it; re-running
`android-release` attaches the files and starts both again.

Run it by hand: *Actions → Nightly release → Run workflow* (`dry_run` prints the version and
highlights and pushes nothing). To skip a night, disable the workflow; to hold a change back, don't
merge it. A broken release is not moved: the next night's run releases the fix as a new patch version,
or run it by hand right after merging the fix. Deploying ([step 8](#release-procedure)) stays a
manual decision.

## Release notes

Rendered by `scripts/render-release-notes.py` from the template `.github/release-notes.md`:

| Part                        | Comes from                                                                                                             |
|-----------------------------|------------------------------------------------------------------------------------------------------------------------|
| Pre-release notice          | tags with a `-` suffix                                                                                                 |
| Highlights                  | the annotated tag's message (a candidate with an empty one: the base version's tag's)                                  |
| Install                     | the template, with the image references of `images.txt`                                                                |
| Upgrading, breaking changes | the template; `### Breaking` of the tag message, else "None."                                                          |
| Known limitations           | the template: unsigned builds, iOS not distributed, push for self-built apps, what is tested in CI only, AppImage size |
| Verify the downloads        | the template: checksums, attestations, cosign                                                                          |
| What's changed              | GitHub's generated notes, grouped by `.github/release.yml`                                                             |

Label pull requests so the generated notes group them: `security`, `breaking`, `feature` /
`enhancement`, `bug` / `fix`, `documentation`, `dependencies`; `skip-release-notes` leaves one out.
Dependabot's pull requests are left out. Edit the template when a known limitation is gone.

## What a release contains

| File                                                               | Built by                                         |
|--------------------------------------------------------------------|--------------------------------------------------|
| `ghcr.io/zeitgleich/teamcraft-server:X.Y.Z` (+ `X.Y`, `latest`)    | `image` jobs (amd64 + arm64, native runners)     |
| `ghcr.io/zeitgleich/teamcraft-sfu:X.Y.Z` (+ `X.Y`, `latest`)       | `image` jobs                                     |
| `ghcr.io/zeitgleich/teamcraft-bridge:X.Y.Z` (+ `X.Y`, `latest`)    | `image` jobs                                     |
| `ghcr.io/zeitgleich/teamcraft-notetaker:X.Y.Z` (+ `X.Y`, `latest`) | `image` jobs                                     |
| `images.txt`                                                       | image references with their digests              |
| `teamcraft-{server,sfu,bridge,notetaker}-X.Y.Z.{spdx,cdx}.json`    | SBOMs of the images (syft; SPDX, CycloneDX)      |
| `teamcraft-source-X.Y.Z.{spdx,cdx}.json`                           | SBOMs of the source tree (Cargo, Gradle)         |
| `TeamCraft_X.Y.Z_*.deb`, `.rpm`, `.AppImage`                       | `desktop-release.yml`, x86_64 and aarch64        |
| `teamcraft-desktop-<arch>.flatpak`                                 | `desktop-release.yml`                            |
| `TeamCraft_X.Y.Z_{aarch64,x64}.dmg`                                | `desktop-release.yml`, Apple silicon and Intel   |
| `TeamCraft_X.Y.Z_{x64,arm64}-setup.exe`, `.msi`                    | `desktop-release.yml`, unsigned                  |
| `TeamCraft_X.Y.Z_{aarch64,x64}.app.tar.gz`                         | `desktop-release.yml`, the macOS update archives |
| `*.sig` next to the AppImages, installers and `.app.tar.gz`        | updater signatures (with the updater key only)   |
| `latest.json`                                                      | updater manifest (`scripts/updater-manifest.py`) |
| `teamcraft-web-X.Y.Z.tar.gz`                                       | the web client (`[web] static_dir`)              |
| `teamcraft-android-X.Y.Z-{full,libre}-release.apk`                 | `android` job, signed with the upload key        |
| `teamcraft-android-X.Y.Z-{full,libre}-release-unsigned.apk`        | instead, when no upload key is configured        |
| `teamcraft-android-X.Y.Z-{full,libre}-debug-signed.apk`            | `android` job; throwaway debug key, testing only |
| `teamcraft-android-X.Y.Z-full-release.aab`                         | `android` job; the bundle for Google Play        |
| `teamcraft-android-X.Y.Z-full-release-unsigned.aab`                | instead, when no upload key is configured        |
| `SHA256SUMS`                                                       | over every file above (Android lines added last) |

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
app inside the mounted dmg. The dmg window is laid out like other Mac apps' (#710): 660 × 400 with
the background of `scripts/brand/dmg_background.py` (committed as `dmg-background.png` and `@2x`;
re-run it after a brand change) and the app and Applications on either side of its arrow; Finder
writes the layout, and where it can't be scripted the job warns and ships the plain window. A broken or missing seal is what Gatekeeper calls "damaged"; an intact
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
| Android  | Release APKs and AAB signed with the upload key when its secrets exist, else `-unsigned`   |
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

## Google Play

`google-play.yml` runs when a release or candidate is published (or by hand: *Actions → Google Play
→ Run workflow* with the tag). It downloads `teamcraft-android-X.Y.Z-full-release.aab` from the
release, checks it against `SHA256SUMS` and uploads it to Google Play with
[r0adkll/upload-google-play](https://github.com/r0adkll/upload-google-play): release name = the
version, "What's new" = the tag's highlights as plain text (`scripts/play-release-notes.py`: without
`### Breaking`, cut to Play's 500 characters with a link to the full notes, the same English text for
every language). The bundle carries the R8 mapping and the Rust library's symbol table, so Play
deobfuscates and symbolicates crashes on its own. Candidates and releases get increasing
`versionCode`s ([Build numbers](#build-numbers)); Play rejects a `versionCode` it has seen, so a
re-run after a successful upload fails at the upload step and changes nothing.

| Setting (repository)                      | Default       | Meaning                                                                   |
|-------------------------------------------|---------------|---------------------------------------------------------------------------|
| Secret `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON` | –             | The service account's JSON key; without it the upload is skipped (notice) |
| Variable `TEAMCRAFT_PLAY_TRACK`           | `internal`    | Track: `internal`, `alpha` (closed), `beta` (open), `production`          |
| Variable `TEAMCRAFT_PLAY_RELEASE_STATUS`  | `draft`       | `draft` (finish it in the Play Console) or `completed` (roll out)         |
| Variable `TEAMCRAFT_PLAY_LOCALES`         | `en-US,de-DE` | Languages of "What's new"; use the languages of the store listing         |

Set up once:

1. **Upload the first bundle by hand.** The Play Developer API only works for an app that has a
   bundle: build `bundleFullRelease` with the upload key (or take the `.aab` of a release), and upload
   it in the Play Console under *Test and release → Testing → Internal testing → Create new release*.
   Play App Signing must be set up with the key first (`apps/android/README.md`, "Signing").
2. **Google Cloud project**: in the [Google Cloud console](https://console.cloud.google.com), pick
   or create a project and enable the **Google Play Android Developer API** (*APIs & Services →
   Library*).
3. **Service account**: *IAM & Admin → Service accounts → Create service account* (no project roles
   needed), then *Keys → Add key → Create new key → JSON*. Store the downloaded file only in the
   secret below and delete it locally (or keep it in the password manager).
4. **Invite it in the Play Console**: *Users and permissions → Invite new users*, the service
   account's e-mail (`…@<project>.iam.gserviceaccount.com`), *App permissions* → add the app →
   **Release to testing tracks** (add *Release to production…* only when production releases should
   be automatic too). Invite, then wait a few minutes until the permission is active.
5. **Secret**: `gh secret set GOOGLE_PLAY_SERVICE_ACCOUNT_JSON < service-account.json`.
6. **Status**: while the app was never published (no review passed yet), Play only accepts `draft`
   releases: each upload then waits in the track as a draft, and *Review release → Start rollout*
   in the Play Console publishes it. Once the app is published, switch to automatic rollout:
   `gh variable set TEAMCRAFT_PLAY_RELEASE_STATUS --body completed`.

Promoting a build to the next track (internal → closed → production) is a Play Console action: the
release on the internal track → *Promote release* → the target track, then review and roll out
(production: staged rollout with a percentage). The bundle is not uploaded again. To send releases to
another track automatically, set `TEAMCRAFT_PLAY_TRACK` (and give the service account the matching
permission); every published tag, candidates included, then goes there.

## Checking locally

```sh
scripts/check-versions.sh v1.0.0-rc.1        # the tag fits the tree
docker run --rm -v "$PWD:/repo" -w /repo docker.io/rhysd/actionlint:1.7.12
echo "ghcr.io/x/teamcraft-server:1.0.0@sha256:0" > /tmp/images.txt
scripts/render-release-notes.py --version 1.0.0 --tag v1.0.0 --repo Zeitgleich/TeamCraft --images /tmp/images.txt
scripts/play-release-notes.py --version 1.0.0 --tag v1.0.0 --repo Zeitgleich/TeamCraft --locales en-US --out-dir /tmp/whatsnew
```

`release.yml` runs on a tag push or by `workflow_dispatch` on a tag ref; to try it without a release,
push a candidate tag (above) or a tag to a fork. `desktop-release.yml` alone can be run by hand from the Actions UI.
