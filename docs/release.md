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
   the Pages site with the update manifests, the APT/RPM repositories and the F-Droid repository of
   the libre Android app (updates.md §8) is rebuilt, and installed
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

To build one part of an existing release again (its job failed, or a fix had to go onto the tag),
run `gh workflow run release.yml --ref vX.Y.Z -f only=android` (or `ios`): only that build and its
attach job run; images, desktop, web and the draft release are left as they are. `only=all` (the
default, and every tag push) builds everything.

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
`nightly-release.yml` releases `main` every night. It has no GitHub schedule, because GitHub starts
scheduled runs hours late or not at all: an external scheduler (the operator's own CI) dispatches it
every day at 04:30 (Europe/Berlin) through the API (`workflow_dispatch` on `main`, with a token
that may only write Actions of this repository). `gh workflow run nightly-release.yml --ref main`
starts it by hand. The steps of [Release procedure](#release-procedure) are only done by hand for a candidate or a prerelease.

| Situation                                                                                | Result                                                                              |
|------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------|
| No commit since the last release tag, or only docs / Markdown                            | nothing (notice in the run)                                                         |
| Every code change since the last tag passed the full CI in its pull request (`ci` label) | no wait for CI on `main`: tagged at once (`scripts/nightly-ci-covered.sh`)          |
| Otherwise, CI of `main` for the newest code change is queued or running                  | run waits for it (at most 90 min), then goes on; red or not done in time: run fails |
| Otherwise, no CI run covers the newest code change (auto-merged PRs)                     | run starts CI on `main` and waits for it                                            |
| CI of `main` is red for the newest code change                                           | run fails, nothing tagged; fix `main`, then run the workflow again                  |
| The last tag has no published release                                                    | run fails: re-run it (`gh workflow run release.yml --ref vX.Y.Z -f publish=true`)   |
| Tree version equals the last tag                                                         | patch bump commit on `main` (`scripts/bump-version.sh`), tag, release `X.Y.(Z+1)`   |
| A pull request raised the tree's version above the last tag                              | the tree is released as it is (minor / major releases: bump in a pull request)      |
| Tree version is a prerelease (`-beta.N`)                                                 | run fails; release by hand                                                          |

The bump commit (`Release X.Y.Z`, signed off by `github-actions[bot]`) is pushed to `main` directly
and carries no skip marker. `main` requires the `ci-gate` check (repository ruleset; see AGENTS.md,
"Merging"), and GitHub Actions can't be exempted from a repository ruleset, so the run pushes the
commit to a helper branch `nightly/bump-X.Y.Z`, marks its `ci-gate` as passed (the bump only changes
version numbers of a tree whose CI was green), pushes it to `main` and deletes the helper branch. The release is what was on `main` when the run
started: the bump commit on that tree is the tagged one. Waiting for CI takes minutes, so `main` may have
moved by then and the push is rejected; the run then puts the same bump on top of the new `main` (up to
five times) and lists the commits merged meanwhile. They stay out of this release (the tag remains on the
checked tree, which then isn't on `main` itself) and go into the next one. The annotated tag's message is the highlights
(`scripts/nightly-notes.py`): the title of every merged pull request (newest first, at most 15, then
"and N more"; `security` and `breaking` ones first), and the `breaking` ones again under
`### Breaking`. Write pull request titles for readers; `skip-release-notes` leaves one out. Labels
(`feature`, `bug`, ...) group the generated list: that list is "What's changed" ([Release notes](#release-notes)).

Pushes made with `GITHUB_TOKEN` start no workflows, so the nightly run starts `release.yml` itself
with `workflow_dispatch` on the tag (`publish: true`). After the images, desktop packages and web
client are built, its `publish` job publishes the release and starts `packages.yml` (update
manifests, APT/RPM), `google-play.yml` and, with an iOS build, `app-store.yml`, which
`release: published` would not do for a token publish. If the Android build failed, the release is published without it; re-running
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
| Known limitations           | the template: unsigned builds, iOS TestFlight only, push for self-built apps, what is tested in CI only, AppImage size |
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

With the secrets `APPLE_CERTIFICATE` (the *Developer ID Application* certificate with its private
key as base64 .p12) and `APPLE_CERTIFICATE_PASSWORD`, the same job signs with the Developer ID, with
hardened runtime and timestamp, notarizes the dmg once (`notarize.sh`; the ticket covers the app in it), staples the dmg and the app
(the update archive packs the stapled app), and requires Gatekeeper to accept them. Notarization uses the App Store Connect API key of the iOS
release (secret `APP_STORE_CONNECT_API_KEY`, variables `APP_STORE_CONNECT_KEY_ID`,
`APP_STORE_CONNECT_ISSUER_ID`, below), or without it `APPLE_ID`, `APPLE_PASSWORD` (an app-specific
password) and `APPLE_TEAM_ID`. The signing identity is read from the certificate;
`APPLE_SIGNING_IDENTITY` only picks one where the .p12 holds several. With the variable
`APPLE_TEAM_ID` set, a signature of another team fails the job.

The Developer ID certificate is not the *Apple Distribution* one of the iOS release: the account
holder creates it once in the developer portal (*Certificates → + → Developer ID Application*, from a
signing request as for iOS), exports it with its key as `.p12` and stores it with
`base64 -i DeveloperID.p12 | gh secret set APPLE_CERTIFICATE` and
`gh secret set APPLE_CERTIFICATE_PASSWORD`. It is valid for five years; apps signed with it keep
working after that.

#### What the signature means for the keychain (#1223)

The desktop app keeps one item in the login keychain, the key of its sealed secrets file. macOS lets
the app that created an item read it and asks the user for the login password when any other app
does. Which app that is, it decides by the designated requirement of the code signature:

| Signature    | Designated requirement                                                        | After an update                    |
|--------------|-------------------------------------------------------------------------------|------------------------------------|
| Ad hoc       | `cdhash H"…"`, the hash of this very build                                    | A stranger: one password dialog    |
| Developer ID | `identifier "de.zeitgleich.teamcraft" and anchor apple generic and … certificate leaf[subject.OU] = <team>` | The same app: no dialog |

`verify-app.sh` prints the requirement and, in a job that has the certificate, fails when it is a
build's hash; without the certificate the job warns. The first Developer ID release after ad-hoc
ones still asks once (the item was made by an ad-hoc build): *Always Allow* adds the signed app to
the item, and no later update asks. An ad-hoc signature with a requirement written by hand
(`identifier "de.zeitgleich.teamcraft"` alone) would also stop the dialogs, but then any program on
the Mac that claims this identifier reads the key, so the release doesn't do that.

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
| iOS      | Signed for the App Store by `ios-release.yml` (below), `.ipa` attached, TestFlight upload  |
| Linux    | Packages are unsigned; checksums and attestations cover them; the repositories sign them   |
| Updates  | minisign signatures for the updater when `TAURI_SIGNING_PRIVATE_KEY` exists (below)        |
| Web      | Signed release manifest (`release.json`) when `TEAMCRAFT_WEB_RELEASE_KEY` exists (below)   |

Updater and repository secrets (how to create them: [updates.md](updates.md) §7):
`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (the public key goes into
`plugins.updater.pubkey` of `apps/desktop/tauri.conf.json`), `LINUX_REPO_GPG_PRIVATE_KEY`,
`LINUX_REPO_GPG_PASSPHRASE`. Without the updater key a release has no `latest.json` and installed apps
don't see it; without the repository key the Pages site has the manifests but no APT/RPM repositories.
The F-Droid repository needs `FDROID_KEYSTORE_BASE64`, `FDROID_KEYSTORE_PASSWORD`, `FDROID_KEY_ALIAS`
and `FDROID_KEY_PASSWORD` ([updates.md](updates.md) §8.3); without them the site has none.

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

## Critical path of the release workflow (#176)

Measured on the nightly runs of 2026-11-17 to 2026-11-20 (v1.0.37 to v1.0.39, 28 to 88 min, usually
about 67 min). Android and iOS don't hold up the draft release (`android-release` and `ios-release`
attach their files later), so the path is: the **Web UI bundle** job, then the desktop jobs that
embed it, then the draft release (about 6 min of assembling).

| Job (v1.0.39)                               | Duration   |
|---------------------------------------------|------------|
| Desktop / Web UI bundle                     | 30 min     |
| Desktop / macOS Intel, Apple silicon        | 32, 30 min |
| Desktop / Windows aarch64, x86_64           | 30, 28 min |
| Desktop / Flatpak x86_64, aarch64 (removed) | 27, 21 min |
| Desktop / Linux x86_64, aarch64             | 24, 17 min |
| Android (not on the path)                   | 47 min     |
| iOS (not on the path)                       | 27 min     |
| Images (not on the path)                    | 3 to 8 min |
| Draft release                               | 6 min      |

What the numbers showed:

- The UI bundle is one long serial compile (wasm, fat LTO: about 20 of its 30 minutes are the
  link of the main bundle, which no dependency cache shortens).
- Every desktop job waited for it (`needs: ui`) and then compiled for 27 minutes: the two added up.
  The Linux, macOS and Windows jobs now **compile their dependencies against a placeholder bundle
  while the UI job runs** (`cargo tauri build --no-bundle`, the same command, config and flags as the
  real build) and then wait for the `ui-dist` artifact
  (`.github/actions/await-ui-bundle`: polls the run's artifacts, fails when the UI job leaves
  `ui-failed`). With the real bundle only the desktop crate is compiled again (its assets are
  embedded with `include_bytes!`, so the changed `index.html` triggers it). Expected: the path becomes
  max(UI bundle, compile) + about 8 min instead of the sum.
- **No Flatpak in the release.** The job only built the Flatpak from source (about 25 min after the UI
  job, 2.6 GB of cache) and attached the bundles. AppImage, deb and rpm cover Linux, and Flathub builds
  the manifest (`apps/desktop/packaging/linux/`) itself once the app is submitted there. The manifest and
  `build-flatpak.sh` stay; the in-app Flatpak support is unchanged.
- The compile no longer needs a warm cache to stay under the UI job (cold: 24 to 28 min against
  30). The Linux jobs therefore don't restore or save a Rust cache any more: their two entries
  (3.3 GB) went over the budget below and were evicted again before the next release.
- Actions cache budget: the warm-up on `main` saved about 13 GB for 10 GB of room, so GitHub evicted
  caches (also the CI ones) between the warm-up and the release (the UI job and macOS missed in v1.0.39).
  Check with `gh cache list`; what the warm-up saves has to stay below about 8 GB.

- **The web client is packaged beside the desktop jobs.** In v1.0.41 the draft release job spent 6.7
  of its 7.5 minutes in "Assemble the release files" after the last desktop job: it compiled the
  segmentation model's NNEF conversion (tract, release profile, no cache) and compressed the web
  bundle with `brotli -q 11`. The desktop workflow's `web` job (plain runner) now converts the model
  at once, waits for `ui-dist` like the macOS and Windows jobs, adds the model, signs, compresses and
  uploads `web-release` (`teamcraft-web-X.Y.Z.tar.gz`); the draft release job only copies it. Expected:
  the draft release job drops to about 1 to 2 minutes (the release was 40.6 min, of which 7.5 after
  the desktop jobs). Confirmed only by the next release.
- **macOS notarization stays on the Mac.** "Notarize the app" and "Notarize the dmg" take 0.7 to 0.8
  minutes each in v1.0.41 and the later runs (about 1.5 minutes per Mac job, 3 minutes of Mac runner
  per release). Submitting without `--wait` and stapling from Linux with `rcodesign` would free
  that, but it needs a second job, an artifact hand-over of the signed dmg and a new tool for under
  5 % of the Mac minutes; not worth the complexity. If Apple's queue gets slow, this is the place.

Cache trade-off: the release jobs restore dependency builds (not the workspace crates) saved by `main`.
Cargo keys them by the exact sources and flags, a hit is a byte-identical rebuild of the same inputs,
and the workspace crates and the final link always build fresh. A cache can only make a release
faster, never different; to get a fully cold build, delete the entries (`gh cache delete`) or run
Desktop release by hand with the cache removed.

Options left, with their cost: larger runners (macOS xlarge about $0.10/min, not free for public
repositories; Windows and Linux larger runners are paid too) would shorten the compile, but the UI job's
LTO link is serial and not helped by more cores. `lto = "thin"` for the web bundle would shorten it
(by about half in the PR that measures it) but makes the shipped bundle larger or slower, so it stays
as it is. Ideas not done: sccache for the C/C++ parts.

## CI dependency mirror and Actions cache

GitHub Free gives the repository 10 GB of Actions cache; the least recently used entry is evicted
when it is full, which used to be the rarely read one that mattered.

- **Rust caches** are saved by runs on `main` only (`save-if`), one key per job. The key ends in the
  hash of `Cargo.lock`, so each dependency change leaves the previous ~1 GB entry behind.
  `cache-prune.yml` (`scripts/prune-caches.sh`) runs after each CI run on `main` and daily: it deletes
  every pull request cache and all but the newest entry of a key family. The vendored/fuzz and
  CodeQL-Android jobs only restore; saving them did not fit the budget. Look at the budget with
  `gh cache list --sort size_in_bytes --json key,sizeInBytes`.
- **GStreamer packages** (Windows installer, macOS runtime and development pkgs) are big and the
  upstream server drops connections. They are not cached; `scripts/fetch-gstreamer.sh` takes them from
  the assets of the `ci-deps` release of this repository, falls back to
  upstream with backoff, and checks every file against `scripts/gstreamer-pins.env`, the one place
  version and hashes are pinned.
- **The `ci-deps` release is not a product release.** Its tag does not match `v*`, so no release
  workflow starts, and it is created with `gh release create ci-deps --prerelease --latest=false`: it
  must never become "Latest" (the desktop updater reads `releases/latest/download/latest.json`).
  The release logic only looks at `v<major>.<minor>.<patch>` tags: `nightly-release.yml` (last tag and
  its release), `scripts/package-site.sh` and `scripts/fdroid-repo.py` (tag patterns), so they skip it. `packages.yml` and `google-play.yml` run on `release: published`, so their jobs
  have an `if` on a `v*` tag: without it creating the mirror release builds the site and fails the
  Pages deploy.
- **Bumping GStreamer:** download the new files from gstreamer.freedesktop.org, compute their SHA-256,
  update `scripts/gstreamer-pins.env`, upload the files to the `ci-deps` release
  (`gh release upload ci-deps <files>`; keep the old ones until the old pins are gone from all
  branches).

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

| Setting (repository)                      | Default       | Meaning                                                                      |
|-------------------------------------------|---------------|------------------------------------------------------------------------------|
| Secret `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON` | –             | The service account's JSON key; without it the upload is skipped (notice)    |
| Variable `TEAMCRAFT_PLAY_TRACK`           | `internal`    | Track of releases: `internal`, `alpha` (closed), `beta` (open), `production` |
| Variable `TEAMCRAFT_PLAY_RC_TRACK`        | `internal`    | Track of candidates (`-rc`), same values; keep it below `production`         |
| Variable `TEAMCRAFT_PLAY_RELEASE_STATUS`  | `draft`       | `draft` (finish it in the Play Console) or `completed` (roll out)            |
| Variable `TEAMCRAFT_PLAY_LOCALES`         | `en-US,de-DE` | Languages of "What's new"; use the languages of the store listing            |

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
permission, "Release to production" for `production`); every published release then goes there,
while candidates stay on `TEAMCRAFT_PLAY_RC_TRACK`.

## TestFlight

`release.yml` builds the iOS app for every tag, like the desktop packages: its job `ios` calls
`ios-release.yml`, which runs on a macOS runner from the tag: the Rust core for the device
(`build-xcframework.sh --device-only`, with line tables for the dSYM), then
`apps/ios/scripts/archive-testflight.sh`, which archives with the **Apple Distribution** certificate
and the App Store provisioning profiles, exports the `.ipa` and uploads the same archive to App Store
Connect (`xcodebuild -exportArchive`, `destination = upload`, authenticated with the App Store Connect
API key). The job `ios-release` then attaches `teamcraft-ios-X.Y.Z.ipa` to the release with its
`SHA256SUMS` line and a build provenance attestation, after `android-release` (both rewrite
`SHA256SUMS`). The `.ipa` is signed for the App Store: it installs only through TestFlight or the App
Store, not by download. A failed iOS build holds neither the release nor Android back; the nightly's
`publish` waits for the attachment. By hand, *Actions → iOS release → Run workflow* with a tag builds
and uploads again without touching the release.

The upload happens when the tag is built, not when the release is published (unlike Google Play):
TestFlight reaches internal testers only, and every tag's build number is new. The submission to the
App Store waits for the published release ([App Store](#app-store)). The build number is the
release's ([Build numbers](#build-numbers)); App Store Connect refuses one it has seen, so a re-run
after a successful upload fails at the upload and changes nothing.
The dSYMs go to Apple with the upload and, with the `CRASH_REPORTS_*` / `DEBUG_FILES_S3_*` secrets,
to the crash reports backend and the private bucket ([Debug symbols](#debug-symbols)); never to a
workflow artifact or the release.

**Signing: certificate plus profiles from the API, no fastlane.** The workflow imports one
distribution certificate into a keychain of the run and downloads the four App Store profiles
through the App Store Connect API (`apple-actions/download-provisioning-profiles`), so a profile
regenerated in the developer portal (after adding a capability) needs no new secret. Alternatives
considered: Xcode's automatic signing on the runner creates a new development certificate on every
fresh runner (Apple mails the team each time, and their number is limited); fastlane match needs a
second private repository for the encrypted certificates, a Ruby toolchain and its own passphrase,
for one app and one certificate. The yearly chore is the same either way: renew the certificate.

| Setting (repository)                               | Meaning                                                                       |
|----------------------------------------------------|-------------------------------------------------------------------------------|
| Variable `APPLE_TEAM_ID`                           | The team that signs (10 characters, *Membership details* in the portal)       |
| Variable `APP_STORE_CONNECT_KEY_ID`                | ID of the App Store Connect API key                                           |
| Variable `APP_STORE_CONNECT_ISSUER_ID`             | Issuer ID shown above the API keys                                            |
| Variable `TEAMCRAFT_IOS_PUSH_GATEWAY_URL`          | Push gateway built into the app (`https://push-gw.zeitgleich.de`); optional   |
| Secret `APP_STORE_CONNECT_API_KEY`                 | The API key's `.p8` text (role *App Manager*)                                 |
| Secret `APPLE_DISTRIBUTION_CERTIFICATE_P12`        | The *Apple Distribution* certificate with its private key, `.p12`, base64     |
| Secret `APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD`   | The `.p12`'s export password                                                  |

Without the three secrets and the three variables nothing is built (a notice) and the release has no `.ipa`. The App
Store Connect API key is not the APNs key: the APNs key (`.p8` too) lives only in the push gateway
([push-gateway.md](push-gateway.md) §6).

Set up once (developer portal and App Store Connect, by an Account Holder or Admin):

1. **Identifiers**: App IDs `de.zeitgleich.teamcraft` (capabilities Push Notifications, App Groups,
   Communication Notifications, Time Sensitive Notifications), `de.zeitgleich.teamcraft.notifications`
   (App Groups, Communication Notifications, Time Sensitive Notifications) and
   `de.zeitgleich.teamcraft.share` and `de.zeitgleich.teamcraft.widget` (App Groups each); the App
   Group `group.de.zeitgleich.teamcraft` assigned to all four. Request *Notification Filtering* for the extension
   (`apps/ios/README.md`).
2. **Certificate**: *Certificates → +* → *Apple Distribution*, with a certificate signing request
   (Keychain Access on a Mac, or `openssl req -new -newkey rsa:2048 -nodes`). Export it with its key
   as `.p12` with a password, then `base64 -i distribution.p12 | gh secret set
   APPLE_DISTRIBUTION_CERTIFICATE_P12` and `gh secret set APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD`.
   Keep the `.p12` in the password manager only.
3. **Profiles**: *Profiles → +* → *App Store Connect*, one for each of the four App IDs, with that
   certificate. Exactly one active App Store profile per bundle ID: the workflow takes the profile's
   name from the portal and stops if there are several.
4. **App record**: App Store Connect → *Apps → +* → *New App*, platform iOS, bundle ID
   `de.zeitgleich.teamcraft`, SKU of your choice.
5. **API key**: App Store Connect → *Users and Access → Integrations → App Store Connect API* →
   *Team Keys → +*, access *App Manager*. Download the `.p8` (only once), then `gh secret set
   APP_STORE_CONNECT_API_KEY < AuthKey_<id>.p8`, `gh variable set APP_STORE_CONNECT_KEY_ID` and
   `gh variable set APP_STORE_CONNECT_ISSUER_ID`, and `gh variable set APPLE_TEAM_ID`.
6. **Testers**: TestFlight → *Internal Testing* group with *automatic distribution*: every processed
   build reaches the team's members at once. External testers (up to 10,000, by invitation or a
   public link) need a *Beta App Review* of the first build of each version.
7. **Export compliance**: answered once under *App Information → App Encryption Documentation*: the
   app uses standard encryption algorithms (in addition to the OS's), not distributed in France for
   now. With these answers App Store Connect needs no documents, so `ITSAppUsesNonExemptEncryption`
   is `false` in `apps/ios/project.yml` and builds don't wait as *Missing Compliance*. Distribution
   in France needs a declaration to the French authority (ANSSI) first; proprietary algorithms would
   change the answers. France is switched off in *Pricing and Availability* until then; the filing, the
   decision and the steps are in [france-cryptology.md](france-cryptology.md).

Renewal: the distribution certificate expires after a year. Make a new one (step 2), regenerate the
four profiles with it (step 3), replace the two certificate secrets, then revoke the old one.

Checked locally on a Mac with the certificate and profiles installed:
`scripts/build-xcframework.sh --device-only`, then `APPLE_TEAM_ID=… TC_PROFILE_APP=…
TC_PROFILE_NOTIFICATIONS=… TC_PROFILE_SHARE=… TC_PROFILE_WIDGET=…
scripts/archive-testflight.sh --no-upload` exports the `.ipa` to `apps/ios/build/export` without
uploading.

### App Store screenshots

The listing's screenshots, a title card and the app's scenes (iPhone 6.3", 1206 × 2622, and iPad 13", 2064 × 2752, which App Store
Connect requires, plus iPhone 6.9", 1320 × 2868; English and German; App Store Connect scales them
down for smaller devices) are made when the listing changes
and are not kept in the repository. They show the Google Play screenshots' scenes and placeholder
texts, drawn as HTML in the iOS app's light theme (`scripts/screenshots/ios-store/scenes.js`, the
app's own icons, a caption above the device) and taken in headless Chrome, as there is no iOS
simulator on Linux. When a screen of the app changes a lot, update its scene.

```sh
scripts/screenshots/ios-store.sh <dir>    # writes <dir>/<en-US|de-DE>/<iphone-6.3|iphone-6.9|ipad-13|play-phone>/*.jpg, header/ and search/ as PNG
```

Upload them by hand in App Store Connect (the version → drag a folder's pictures onto the iPhone
and iPad boxes per language), or with the TestFlight upload's API key, which replaces the 6.3", 6.9"
and 13" screenshots of every language the version in preparation has:

```sh
ASC_KEY_ID=… ASC_ISSUER_ID=… ASC_KEY_PATH=AuthKey_….p8 apps/ios/scripts/upload-screenshots.py <dir>
```

`--dry-run` checks the pictures' sizes without uploading. `play-phone/` holds the same scenes on
a Pixel (Android status bar and camera hole, 1080 × 2400) for the Google Play listing's phone
screenshots, uploaded by hand in the Play Console. The product page's creative assets,
`header/` (5244 × 2950, the brand and the claim in its middle, as the sides are cut off on narrow
screens) and `search/` (3840 × 2560, three phones with the app), go in by hand: the version →
*Header and search results*. Needs Node 22+, Chrome, python3 with
Pillow (and `cryptography` for the upload), and the fonts Inter and Noto Color Emoji.

The English texts of the listing (name, subtitle, promotional text, description, keywords) are in
`apps/ios/store/en-US/`; the German ones are kept in App Store Connect. A language has to be added to
the version in App Store Connect (*+* next to the language) before `upload-screenshots.py` fills it.

## App Store

`app-store.yml` sends a release's iOS build from TestFlight to App Review, the way `google-play.yml`
sends the Android bundle to Play: when a release is published (the nightly's `publish` job starts it,
like Google Play), or by hand (*Actions → App Store → Run workflow* with the tag). It runs
`apps/ios/scripts/app-store-submit.py` on Linux with the App Store Connect API key of the TestFlight
upload (no fastlane; the API client `apps/ios/scripts/asc_api.py` is shared with the screenshot
upload), which:

1. waits until App Store Connect has processed the build of the release's build number (a few
   minutes to an hour; it gives up after 90 minutes, then run the workflow again);
2. answers export compliance if the build still asks (it normally doesn't:
   `ITSAppUsesNonExemptEncryption` is `false`, see [TestFlight](#testflight), step 7);
3. creates the iOS App Store version `X.Y.Z`, which copies the listing (texts, screenshots, review
   details) from the previous version, or reuses the version in preparation (renamed to `X.Y.Z`);
4. attaches the build, sets the release type (*automatically after approval*) and "What's new" in
   every language of the listing: the tag's highlights as plain text (`scripts/play-release-notes.py
   --max-length 4000`, the same English text as on Play). The first version of an app has no "What's
   new";
5. submits the version for App Review (a review submission with that version).

Candidates (`vX.Y.Z-rc.N`) stay in TestFlight. The job summary says what happened.

**One version at a time.** App Store Connect has at most one version in preparation or review. Releases
come every night, a review takes about a day, so a release often finds the previous one still waiting:

| State of the version in flight                              | Result                                                                                                                           |
|-------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------|
| none (the previous one is live)                             | new version, submitted                                                                                                           |
| in preparation, or rejected                                 | that version, renamed to this release, with this build, submitted                                                                |
| waiting for review, `TEAMCRAFT_APP_STORE_WHEN_PENDING=skip` | left alone (warning); this release stays in TestFlight, the next one after it is live goes                                       |
| waiting for review, `replace`                               | its submission is withdrawn, the version gets this release's number and build, submitted again (it loses its place in the queue) |
| this release with this build already waiting                | nothing (a re-run)                                                                                                               |
| in review, approved and not yet live                        | left alone (warning); never touched                                                                                              |
| this release (or a newer one) already live                  | nothing                                                                                                                          |

`skip` is the default: withdrawing every night could keep a version from ever reaching a reviewer.

| Setting (repository)                        | Default          | Meaning                                                                             |
|---------------------------------------------|------------------|-------------------------------------------------------------------------------------|
| Variable `TEAMCRAFT_APP_STORE_MODE`         | `submit`         | `submit`; `prepare` (version, build and notes, submitted by hand); `off`            |
| Variable `TEAMCRAFT_APP_STORE_WHEN_PENDING` | `skip`           | A version waiting for review: `skip` this release, or `replace` it                  |
| Variable `TEAMCRAFT_APP_STORE_RELEASE_TYPE` | `AFTER_APPROVAL` | `AFTER_APPROVAL` goes live once approved; `MANUAL` waits for *Release this version* |
| The API key of [TestFlight](#testflight)    | –                | Without it the run ends with a notice                                               |

By hand, *Run workflow* has `dry_run` (only reads App Store Connect and prints every change it would
make, in order, with its request body) and `when_pending` (overrides the variable for this run). The
same on a computer with the key: `ASC_KEY_ID=… ASC_ISSUER_ID=… ASC_KEY_PATH=AuthKey_….p8
apps/ios/scripts/app-store-submit.py --version X.Y.Z --dry-run` (python3 with `cryptography`).

Set up once, in App Store Connect: the listing of the first version complete (screenshots, texts,
privacy, age rating, App Review contact and sign-in), since every new version copies it; the API key
needs the role *App Manager* (it has it for TestFlight). Screenshots are changed in the version in
preparation ([App Store screenshots](#app-store-screenshots)) and carried over from then on.

## Debug symbols

Shipped binaries carry no debug info and no symbol table (smaller downloads). Every release build
keeps them separately, by build ID, so a crash report or a stack sample of a shipped build can be
symbolized later, the way Google Play keeps the R8 mapping and native symbols per build.
`scripts/debug-files.sh` does the work in `desktop-release.yml` and the `android` job of
`release.yml`.

| Platform | Built with                                | Shipped binary                             | Debug file                                 |
|----------|-------------------------------------------|--------------------------------------------|--------------------------------------------|
| Linux    | line tables, not stripped                 | stripped by `debug-files.sh split`         | `teamcraft-desktop.debug` (zlib), build ID |
| macOS    | line tables, `split-debuginfo = "packed"` | `strip -S -x` after the UUID check         | `teamcraft-desktop.dSYM`, Mach-O UUID      |
| Windows  | line tables, `strip = "none"`             | never holds symbols (only the PDB's GUID)  | `teamcraft_desktop.pdb`, GUID and age      |
| Android  | line tables                               | stripped by AGP in the APKs and the bundle | the unstripped `.so` files, build ID       |
| Web      | wasm-opt `-g` (name section)              | name section taken out by the bundle tool  | `teamcraft-ui-names-<build>.json.gz`       |


**Never a workflow artifact:** this repository is public, and so are its Actions artifacts. The
debug files go only to the places below; with none configured, the step warns and the release goes
on without them. A failed upload warns too.

| Where                                   | Configured by (repository secrets)              | What it is for                                                     |
|-----------------------------------------|-------------------------------------------------|--------------------------------------------------------------------|
| Crash reports backend (GlitchTip ≥ 7.0) | `CRASH_REPORTS_*` (crash-reports.md, "Symbols") | Symbolizes native crash reports by debug ID; no download           |
| Private S3 bucket                       | `DEBUG_FILES_S3_*` (below)                      | The place to download them from: `eu-stack`, `gdb`, `atos`, WinDbg |

The bucket is any S3 service (path-style URLs, SigV4; `curl --aws-sigv4`, no SDK), set by the
secrets `DEBUG_FILES_S3_ENDPOINT` (`https://s3.example.com`), `DEBUG_FILES_S3_BUCKET`,
`DEBUG_FILES_S3_ACCESS_KEY_ID`, `DEBUG_FILES_S3_SECRET_ACCESS_KEY` and `DEBUG_FILES_S3_REGION`
(default `us-east-1`). Give the key read and write on that bucket only, keep the bucket private,
and set a lifecycle rule if old builds should expire (a nightly release adds a few hundred MB).
Layout, the one each platform's tools use:

| Key                                       | Content                                                        |
|-------------------------------------------|----------------------------------------------------------------|
| `buildid/<build-id>/debuginfo`            | ELF debug file (Linux, Android), as a debuginfod server has it |
| `macho/<UUID>/teamcraft-desktop.dSYM.zip` | dSYM bundle                                                    |
| `pdb/<name>.pdb/<GUID><age>/<name>.pdb`   | PDB, as a symbol server (symstore) has it                      |
| `index/desktop/<version>/<target>.txt`    | The build's files with their IDs, and the commit               |
| `index/android/<version>.txt`             | The same for the Android libraries                             |

A build that isn't a tag (Actions → Desktop release → Run workflow) is indexed as
`<version>+<commit>`.

### Symbolizing a Linux stack

The bucket's credentials go into the environment (`DEBUG_FILES_S3_*`, as above); `scripts/debug-files.sh
fetch` puts the debug file where elfutils and gdb look by build ID.

1. **The build ID** of the shipped executable (deb/rpm: `/usr/bin/teamcraft-desktop`; AppImage: the
   running process, or `--appimage-extract`):

   ```sh
   eu-readelf -n /usr/bin/teamcraft-desktop | grep 'Build ID'
   eu-unstrip -n -p "$(pidof teamcraft-desktop)" | grep teamcraft-desktop   # of a running process
   coredumpctl info teamcraft-desktop                                       # of a core dump
   ```

   Or look it up by version: `scripts/debug-files.sh index desktop/1.0.32/x86_64-unknown-linux-gnu`.
2. **Fetch it** (to `~/.cache/teamcraft-debug/.build-id/<xx>/<rest>.debug` unless a directory is given):

   ```sh
   scripts/debug-files.sh fetch 8bb2f9efa81511d2e8212b7e19b513864e5c4920
   ```

3. **Live stack samples** (idle CPU, hangs), with inlined frames, file and line:

   ```sh
   eu-stack -p "$(pidof teamcraft-desktop)" -i -s --debuginfo-path="$HOME/.cache/teamcraft-debug" \
     | rustfilt
   ```

   Set `DEBUGINFOD_URLS=` (empty) to keep elfutils from asking the distribution's debuginfod for it.
   Names are Rust v0 mangled (`_RNv…`); `rustfilt` (`cargo install rustfilt`) demangles them.
4. **A single address** from a report: subtract the executable's load address (the first
   `teamcraft-desktop` line of `/proc/<pid>/maps`, or the report's image address) and look up the
   offset:

   ```sh
   eu-addr2line -e ~/.cache/teamcraft-debug/.build-id/8b/b2f9….debug -f -i -C 0x1449e
   ```

5. **A core dump:**

   ```sh
   coredumpctl debug teamcraft-desktop \
     --debugger-arguments="-iex 'set debug-file-directory /usr/lib/debug:$HOME/.cache/teamcraft-debug'"
   ```

   Or copy the file to `/usr/lib/debug/.build-id/<xx>/<rest>.debug`, where gdb and every elfutils
   tool find it without options.

### Symbolizing on macOS, Windows and Android

- **macOS:** `dwarfdump --uuid /Applications/TeamCraft.app/Contents/MacOS/teamcraft-desktop` gives
  the UUID; download `macho/<UUID>/teamcraft-desktop.dSYM.zip` (UUID without dashes) from the
  bucket, unzip it, and `atos -o teamcraft-desktop.dSYM -l <load address> <address>`. A dSYM next
  to the app (or found by Spotlight) is picked up by lldb and Console crash reports.
- **Windows:** the bucket's `pdb/` tree is a symbol store: copy the PDB into a local directory in
  the same layout and set `_NT_SYMBOL_PATH=srv*C:\symbols` (WinDbg, Visual Studio), or put it next to
  the executable.
- **Android:** Google Play symbolizes from the bundle's symbol table ([Google Play](#google-play)). For a
  tombstone or `adb bugreport`, the library's build ID is in the backtrace lines (`BuildId: …`):
  fetch it as on Linux and run `ndk-stack -sym <dir>` or `llvm-addr2line` on the offsets.

### Crash reports

GlitchTip (from 7.0) symbolizes a native frame server-side when the event carries the frame's
`instruction_addr` and the image's debug ID in `debug_meta.images`: Android's NDK crashes do. The
desktop app's panic reports don't yet: they resolve frames on the device from `std::backtrace`, which
finds no names in a stripped binary, so they show the panic's message and location only
(crash-reports.md, "Symbols").

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
