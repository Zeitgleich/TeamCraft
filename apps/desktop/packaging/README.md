# Desktop packaging

What the build produces and how to build it. Nothing here publishes anything.

| Platform | Formats                            | Where it is configured                                              |
|----------|------------------------------------|---------------------------------------------------------------------|
| Linux    | Flatpak (primary)                  | `linux/de.zeitgleich.teamcraft.yml`, `.desktop` file, AppStream data  |
| Linux    | `.deb`, `.rpm`, AppImage           | `bundle.linux` in `tauri.conf.json`                                 |
| macOS    | `.dmg` (arm64 and x86_64)          | `bundle.macOS`, `Info.plist`, `macos/Entitlements.plist`            |
| Windows  | NSIS `.exe` and `.msi` (x64, arm64) | `tauri.windows.conf.json`, [`windows/README.md`](windows/README.md) |

## Build locally

Prerequisites: Rust with the `wasm32-unknown-unknown` target, [Trunk](https://trunkrs.dev), the Tauri CLI
(`cargo install tauri-cli --version "^2" --locked`), and on Linux the WebKitGTK 4.1 / GTK 3 development
packages (see the README of `teamcraft-ui`). The `beforeBuildCommand` builds the UI with Trunk first.

```sh
cd apps/desktop
cargo tauri build --bundles deb,rpm,appimage      # Linux; results in target/release/bundle/
cargo tauri build --bundles dmg                   # macOS only
cargo tauri build --bundles nsis,msi              # Windows only (see windows/README.md)
cargo tauri build --no-bundle                     # just target/release/teamcraft-desktop
```

Package metadata (name, description, license, category) comes from `tauri.conf.json`. The package is
called `team-craft` on Debian and Fedora because the Tauri bundler derives it from the product name.
Both `.deb` and `.rpm` pull in WebKitGTK and GTK automatically and recommend a keyring service
(`gnome-keyring`) because the refresh token is stored there.

## Updates

Windows, macOS and AppImage builds update themselves; deb and rpm come from the signed APT and RPM
repositories; Flatpak through Flatpak ([docs/updates.md](../../../docs/updates.md)). The release
workflow signs the update artifacts with `sign-update.sh` when the updater key secret exists. Local
builds have an empty `plugins.updater.pubkey` unless it is committed, and debug builds never update.

## Title bar

TeamCraft's top bar is the window's title bar: no native decorations on Windows and Linux (own
window controls, in the order the desktop's button layout gives), the real traffic lights over the
top bar on macOS. Users switch to the system title bar in Settings → Desktop ("Use the system title
bar"); it applies at once on Windows and Linux and at the next start on macOS. A package or a
machine policy can make the system title bar the default with `TEAMCRAFT_TITLE_BAR=system` (for
Flatpak: `flatpak override --env=TEAMCRAFT_TITLE_BAR=system de.zeitgleich.teamcraft`); once the
user has saved the desktop settings, the saved value wins. Windows 11 snap layouts don't open on
hovering the custom maximize button (Tauri can't do that yet, tauri-apps/tauri#4531; tracked in
#173); Win+Z, Win+arrow keys and dragging to the screen edges work as usual.

## Icons

`icons/` holds the app icon: PNGs from 32 px to 1024 px and `icon.icns` for macOS (PNG payloads for
16–1024 px). The glyph is a placeholder; replace `icon-1024.png` with the real artwork and regenerate the
smaller sizes and the `.icns` (for example with `cargo tauri icon icons/icon-1024.png`).

## Flatpak

Flatpak is the primary Linux channel. Building, permissions, video in the sandbox, which package
suits which distribution and the Flathub checklist are in [linux/README.md](linux/README.md); the
Wayland test checklist is [linux/MANUAL-TEST.md](linux/MANUAL-TEST.md).

## macOS

- `Info.plist` adds the camera and microphone usage descriptions; the bundler merges it into the app.
- `macos/Entitlements.plist` grants camera, microphone and network access under the hardened runtime
  (`hardenedRuntime` is on). The listener entitlement covers the loopback redirect of the login.
- There is a dmg per architecture (Apple silicon and Intel; no universal build, which carried the
  native code twice). Hardware H.264 uses GStreamer's VideoToolbox encoder:
  the release workflow installs the official universal GStreamer framework, links against it, and
  `macos/bundle-gstreamer.sh` builds a slim copy: only the four plugins the encoder pipeline uses (its
  `PLUGINS`), the plugin scanner and the libraries they and the executable load, thinned to the
  executable's architecture and stripped (about 32 MB for both architectures together, instead of more than 1 GB for the installed runtime and development files). It rewrites install names
  and search paths to the app's `Frameworks` directory, re-signs ad hoc and verifies that every reference
  resolves (`tauri.gstreamer.conf.json` adds the copy to the bundle). A new element in the pipeline needs
  its plugin added to `PLUGINS`. Compile- and link-checked in CI only; whether the relocated framework finds its plugins at
  runtime needs a real Mac, and without it video falls back to OpenH264. The disable-library-validation
  entitlement is for the bundled framework.
- The app is signed inside out and checked before the dmg is made from it (`macos/sign-app.sh`,
  `macos/make-dmg.sh`, `macos/verify-app.sh`): ad hoc without a Developer ID, with it (secrets listed in
  `docs/release.md`) also notarized and stapled.

## Release workflow

`.github/workflows/desktop-release.yml` runs on manual dispatch, or is called by `release.yml` when a tag
like `v1.0.0` is pushed. It builds the web UI once, then the Linux packages, a Flatpak bundle (from
source inside the Flatpak builder container), the macOS dmgs (Apple silicon and Intel, one job each) and
the Windows installers (x64 and arm64, each on a native runner), all embedding that UI, and keeps them as
workflow artifacts. `desktop-cache.yml` runs the same compile on `main` so the tag's release run starts
from a warm Rust build cache.
`release.yml` attaches them to the draft GitHub release ([docs/release.md](../../../docs/release.md));
nothing is published to any store.
