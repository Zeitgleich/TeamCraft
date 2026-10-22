# Desktop packaging

What the build produces and how to build it. Nothing here publishes anything.

| Platform | Formats                            | Where it is configured                                              |
|----------|------------------------------------|---------------------------------------------------------------------|
| Linux    | `.deb`, `.rpm`, AppImage           | `bundle.linux` in `tauri.conf.json`                                 |
| Linux    | Flatpak (draft)                    | `linux/org.teamcraft.desktop.yml`, `.desktop` file, AppStream data  |
| macOS    | `.dmg` (arm64 and x86_64)          | `bundle.macOS`, `Info.plist`, `macos/Entitlements.plist`            |

Windows is not planned (see PLAN.md, platform table).

## Build locally

Prerequisites: Rust with the `wasm32-unknown-unknown` target, [Trunk](https://trunkrs.dev), the Tauri CLI
(`cargo install tauri-cli --version "^2" --locked`), and on Linux the WebKitGTK 4.1 / GTK 3 development
packages (see the README of `teamcraft-ui`). The `beforeBuildCommand` builds the UI with Trunk first.

```sh
cd apps/desktop
cargo tauri build --bundles deb,rpm,appimage      # Linux; results in target/release/bundle/
cargo tauri build --bundles dmg                   # macOS only
cargo tauri build --no-bundle                     # just target/release/teamcraft-desktop
```

Package metadata (name, description, license, category) comes from `tauri.conf.json`. The package is
called `team-craft` on Debian and Fedora because the Tauri bundler derives it from the product name.
Both `.deb` and `.rpm` pull in WebKitGTK and GTK automatically and recommend a keyring service
(`gnome-keyring`) because the refresh token is stored there.

## Icons

`icons/` holds the app icon: PNGs from 32 px to 1024 px and `icon.icns` for macOS (PNG payloads for
16–1024 px). The glyph is a placeholder; replace `icon-1024.png` with the real artwork and regenerate the
smaller sizes and the `.icns` (for example with `cargo tauri icon icons/icon-1024.png`).

## Flatpak

The manifest consumes the release binary instead of building from source, so it needs neither a Rust
toolchain nor network access while building:

```sh
cd apps/desktop && cargo tauri build --no-bundle && cd ../..
flatpak-builder --user --install --force-clean build-dir apps/desktop/packaging/linux/org.teamcraft.desktop.yml
flatpak run org.teamcraft.desktop
```

Permissions (see `finish-args`): network (server and the loopback listener of the OIDC login), Wayland and
X11, GPU, camera (`--device=all` until the camera portal is used), PulseAudio, the Secret Service (refresh
token) and notifications. Opening the login page in the system browser goes through the OpenURI portal.
The runtime is GNOME 48, which ships WebKitGTK 4.1. Submitting to Flathub needs a source-based manifest
(vendored crates) and screenshots in the AppStream file; both are still to do.

## macOS

- `Info.plist` adds the camera and microphone usage descriptions; the bundler merges it into the app.
- `macos/Entitlements.plist` grants camera, microphone and network access under the hardened runtime
  (`hardenedRuntime` is on). The listener entitlement covers the loopback redirect of the login.
- Signing and notarization are not configured. The release workflow builds an unsigned dmg; the
  `APPLE_*` secrets it would need are listed in the workflow file.

## Release workflow

`.github/workflows/desktop-release.yml` runs on manual dispatch or when a tag like `v0.1.0` is pushed. It
builds the Linux packages, a Flatpak bundle (inside the Flatpak builder container) and the macOS dmg for
both architectures, and keeps them as workflow artifacts. It does not create a GitHub release and does not
publish to any store.
