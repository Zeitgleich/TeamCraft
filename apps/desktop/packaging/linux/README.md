# Linux packages

Flatpak is the primary channel: one build for every distribution, with the GNOME runtime's
WebKitGTK, GStreamer and PipeWire, updated independently of the distribution. The `.deb`, `.rpm`
and AppImage from `cargo tauri build` stay available for distributions and fleets that prefer
native packages.

Every format is built for **x86_64 and aarch64** (arm64: Ampere/Graviton workstations, Snapdragon
X laptops on Linux, Raspberry Pi 4/5 and other ARM boards with a 64-bit OS), each on a native
GitHub runner. The packages carry the architecture in their file names (`_amd64.deb`/`_arm64.deb`,
`.x86_64.rpm`/`.aarch64.rpm`, `_amd64.AppImage`/`_aarch64.AppImage`, and one Flatpak bundle per
architecture).

### What differs on aarch64

| Piece                     | aarch64                                                                                            |
|---------------------------|----------------------------------------------------------------------------------------------------|
| OpenH264                  | Cisco publishes `libopenh264-2.6.0-linux-arm64.8.so`; downloaded and verified like on x86_64        |
| Hardware encoding         | VA-API where the GPU has it (AMD dGPU in an arm64 workstation); otherwise V4L2 encoders (`v4l2h264enc`: Raspberry Pi 4, Rockchip); else OpenH264. The Raspberry Pi 5 has no H.264 encoder |
| Echo cancellation         | WebRTC's audio processing builds for aarch64 (NEON)                                                |
| Segmentation, effects     | tract uses NEON; the effects need a Vulkan GPU (Mesa: Panfrost/PanVK, V3DV, Freedreno/Turnip)       |
| Flatpak runtime           | GNOME 51 and the GL extension exist for aarch64 on Flathub                                         |

## Which package for which distribution

| Distribution                        | Recommended          | Also works                       | Notes                                                                                                                       |
|-------------------------------------|----------------------|----------------------------------|-----------------------------------------------------------------------------------------------------------------------------|
| Fedora Workstation / KDE (42+)      | Flatpak              | `.rpm`                           | Flatpak and Flathub are set up out of the box                                                                               |
| Debian 13 (trixie) and newer        | Flatpak              | `.deb`, AppImage                 | `apt install flatpak` and add Flathub once                                                                                  |
| Ubuntu 25.04 and newer              | Flatpak              | `.deb`, AppImage                 | Ubuntu ships Snap, not Flatpak: `apt install flatpak` and add Flathub                                                       |
| Ubuntu 24.04 LTS, Debian 12         | Flatpak              | –                                | The native packages are built on Debian 13 and need glibc 2.41 and WebKitGTK 4.1 from it; the Flatpak brings its own       |
| Linux Mint, Pop!_OS, elementary OS  | Flatpak              | `.deb` (if based on Ubuntu 25.04+) | Flathub is preinstalled on Mint and Pop!_OS                                                                               |
| Arch Linux, Manjaro, EndeavourOS    | Flatpak              | AppImage                         | No official AUR package yet; an AUR `teamcraft-bin` PKGBUILD can repackage the `.deb` (dependencies: `webkit2gtk-4.1`, `gstreamer`, `gst-plugins-base-libs`, `gst-plugins-bad` for VA-API) |
| openSUSE Tumbleweed / Leap          | Flatpak              | AppImage                         | The `.rpm` names Fedora's package names (`gstreamer1`, `webkit2gtk4.1`), which openSUSE calls differently                  |
| RHEL / AlmaLinux / Rocky 10         | Flatpak              | `.rpm` (with EPEL)               | WebKitGTK 4.1 must be available                                                                                             |
| Immutable (Silverblue, Kinoite, Bazzite, SteamOS) | Flatpak | –                                 | The only sensible option                                                                                                    |

## What each package needs from the system

| Package  | Brings along                                       | Needs from the system                                                                                   |
|----------|----------------------------------------------------|---------------------------------------------------------------------------------------------------------|
| Flatpak  | GNOME 51 runtime (WebKitGTK, GStreamer, PipeWire client libs); Mesa through the GL extension | Flatpak ≥ 1.14, `xdg-desktop-portal` with a desktop backend (GNOME, KDE, wlr), PipeWire, a Secret Service (GNOME Keyring, KWallet/ksecretd) |
| `.deb`   | –                                                  | WebKitGTK 4.1, GTK 3, GStreamer 1.x (+ `gstreamer1.0-plugins-bad` for VA-API), PipeWire, ALSA            |
| `.rpm`   | –                                                  | The same, with Fedora package names                                                                       |
| AppImage | WebKitGTK and GTK                                  | GStreamer (not bundled: `bundleMediaFramework` is off), glibc ≥ 2.41, PipeWire, a Secret Service          |

All of them download Cisco's OpenH264 on first video use (see `crates/teamcraft-media/README.md`)
and use the desktop's portals for screen sharing, the camera, push-to-talk on Wayland, file dialogs
and opening the login page.

## Start-up environment

Before GTK starts, the shell checks two variables that GTK and WebKitGTK read only once
(`apps/desktop/src/startup_env.rs`). If one is missing, the app restarts itself once with it set,
so this works the same for every package (deb, rpm, AppImage, Flatpak) and from a terminal. A value
you set yourself is never changed.

| Variable                           | Set to | Why                                                                                     |
|------------------------------------|--------|-----------------------------------------------------------------------------------------|
| `GTK_USE_PORTAL`                   | `1`    | File dialogs through xdg-desktop-portal, the desktop's own dialog (not in Flatpak, which does this anyway) |
| `WEBKIT_DISABLE_DMABUF_RENDERER`   | `1`    | Rendering, see below                                                                    |

### Rendering

WebKitGTK's DMABUF renderer makes transitions and animations flicker, or flash the whole window
dark, on some GPU drivers and compositors (seen with AMD GPUs on KDE Plasma, #687). TeamCraft
therefore starts WebKitGTK with `WEBKIT_DISABLE_DMABUF_RENDERER=1`: pages are handed to the
compositor through shared memory instead. It looks the same and costs a little more CPU time while
something moves.

To keep the DMABUF renderer (for example to compare, or where it works well), start the app with
either variable:

```sh
TEAMCRAFT_WEBKIT_DMABUF=1 teamcraft-desktop          # the app leaves WebKit's default alone
WEBKIT_DISABLE_DMABUF_RENDERER=0 teamcraft-desktop   # WebKit's own switch, kept as you set it
TEAMCRAFT_WEBKIT_DMABUF=1 ./TeamCraft_amd64.AppImage    # the same for the AppImage
flatpak run --env=TEAMCRAFT_WEBKIT_DMABUF=1 de.zeitgleich.teamcraft
```

For a permanent choice, put the variable into the `.desktop` file's `Exec=env …` line or
`~/.config/environment.d/`, or use `flatpak override --user --env=TEAMCRAFT_WEBKIT_DMABUF=1
de.zeitgleich.teamcraft`.

### Dropped files

WebKitGTK hands the page a file dragged from a file manager (Dolphin, GNOME Files) without its
contents: no `File` objects, at most the `file://` addresses. The shell watches the same drop on
the GTK widget (X11 and Wayland alike), keeps the dropped paths, and the page reads exactly those
files from it in chunks when its `drop` arrives (`apps/desktop/src/dropped_files.rs`, #651). Should
no `drop` reach the page, the shell tells it after half a second and the files go to the
conversation or thread the pointer was over. A dropped file larger than 512 MiB is not read this
way (the page would hold it in memory); attach it with the + button.

## Flatpak

### Build and run locally

```sh
flatpak install --user flathub org.flatpak.Builder org.gnome.Sdk//51 org.gnome.Platform//51 \
  org.freedesktop.Sdk.Extension.rust-stable//26.08 org.freedesktop.Sdk.Extension.llvm22//26.08
apps/desktop/packaging/linux/build-flatpak.sh --run      # build and start in the sandbox
apps/desktop/packaging/linux/build-flatpak.sh --bundle   # build a single-file .flatpak
```

`build-flatpak.sh` first builds the web UI with Trunk in the CI's Rust container (`build-ui.sh`;
the Flatpak SDK's Rust has no wasm32 target), then the app inside the GNOME SDK. Build state goes to
`~/.cache/teamcraft-flatpak` (about 6 GB), never to `/tmp`. With `--ccache` (which the script
passes) the manifest keeps the cargo target and home in flatpak-builder's ccache directory, so a
rebuild compiles only what changed.

In the release workflow (`desktop-release.yml`) the Flatpak jobs restore the flatpak-builder state
that the warm-up on `main` (`desktop-cache.yml`) saved, keyed on `Cargo.lock` and this manifest; the
warm-up drops the workspace crates from the cargo target before saving, like rust-cache.

### Permissions

| Permission                                   | Why                                                                     |
|----------------------------------------------|-------------------------------------------------------------------------|
| `--share=network`                            | The server, and the loopback listener of the OIDC login                 |
| `--socket=wayland`, `--socket=fallback-x11`  | The window; X11 only where Wayland isn't available                      |
| `--device=dri`                               | GPU: WebKit, the effects shaders, VA-API encoding                        |
| `--socket=pulseaudio`                        | Microphone and speakers (PipeWire serves the PulseAudio protocol)        |
| `--filesystem=xdg-run/pipewire-0`            | Screen shares with sound (see below)                                     |
| `--talk-name=org.freedesktop.secrets`        | The refresh token in the desktop keyring                                 |
| `--talk-name=org.freedesktop.Notifications`  | Mention and direct-message notifications                                 |

No home directory, no `--device=all`: the camera, screen sharing, push-to-talk, file dialogs and
the browser all go through portals (Camera, ScreenCast, GlobalShortcuts, FileChooser, OpenURI),
which ask the user and need no static permission.

**The PipeWire socket** (`xdg-run/pipewire-0`) is the one broad permission: the ScreenCast portal
carries no sound, so a share's sound is captured by linking other applications' playback streams
into the app's own capture stream through PipeWire directly. The socket is granted for the app's
whole lifetime, not only during a share, and it lets the app see the PipeWire graph (which
applications play sound) and connect to any node. The app only uses it while the user shares
with sound turned on, links playback streams only (never microphones), and leaves out its own
process and every process it started (the WebView's helpers). Without the permission (removed
with `flatpak override --nofilesystem=xdg-run/pipewire-0 de.zeitgleich.teamcraft`) everything
works except the "with sound" option, which then says it is unavailable.

### Video inside the sandbox

- **Hardware encoding:** GStreamer's `vah264enc` from the runtime, with the VA-API driver from
  `org.freedesktop.Platform.GL.default` (Mesa: AMD, and Intel through `iHD`/`i965` where the
  runtime provides them). `--device=dri` gives access to the render node. Without a usable
  encoder the app encodes with OpenH264.
- **OpenH264:** downloaded from Cisco into `~/.var/app/de.zeitgleich.teamcraft/data/de.zeitgleich.teamcraft/codecs/`
  and checked against Cisco's hashes. (The runtime's `org.freedesktop.Platform.openh264`
  extension is not used: its version is not one the `openh264` crate verifies.)
- **Segmentation model:** bundled at `/app/lib/TeamCraft/models/` (Tauri's resource directory for
  `/app/bin`), fetched by flatpak-builder with its SHA-256 pinned in the manifest.

### Checks

- `portal-test/run.sh`: screen sharing through the ScreenCast portal on a headless Wayland session
  (sway, xdg-desktop-portal-wlr, PipeWire) in a container. Passes: 30 frames at 1920x1080.
- `MANUAL-TEST.md`: the GNOME and KDE checklist (multi-monitor, window capture, camera dialogs,
  push-to-talk, keyring, notifications), to run on real sessions.

### Flathub submission (not submitted)

- [x] **App id.** `de.zeitgleich.teamcraft` (Flathub wants an id under a domain the project controls).
      Re-run the linter and confirm `appid-url-not-reachable` and `appid-ends-with-lowercase-desktop`
      are gone.
- [ ] **Offline build.** Remove `build-args: --share=network`; generate `cargo-sources.json` with
      `flatpak-builder-tools/cargo/flatpak-cargo-generator.py Cargo.lock` and add it to `sources`,
      with `CARGO_NET_OFFLINE=true` and `cargo --offline`.
- [ ] **UI bundle.** Attach `teamcraft-ui-dist-<version>.tar.gz` (from `build-ui.sh`) to each
      release and reference it as an `archive` source with its SHA-256, instead of the local `dist`.
- [ ] **Sources from a tag.** Replace the `dir` source with a `git` source pinned to the release
      tag and commit.
- [ ] **Audio processing.** The bundled WebRTC audio processing build uses meson/ninja from the SDK
      and its own downloads; vendor its subprojects for the offline build.
- [ ] **AppStream.** Screenshots (`<screenshots>` with captions), a `<release>` per version with
      notes, `<branding>` colors, `<content_rating>` reviewed; `appstreamcli validate --explain`
      and `flatpak-builder-lint appstream` pass.
- [ ] **Manifest lint.** `flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest
      <manifest>` passes, or each exception is justified in the PR (talk-names).
- [ ] **Notifications.** Consider the Notification portal instead of `--talk-name=org.freedesktop.Notifications`
      (Flathub reviewers ask about talk-names).
- [ ] **License and trademarks.** AGPL-3.0-or-later in metainfo; the OpenH264 download is runtime-only
      and mentioned in the description; third-party notices shipped (`crates/teamcraft-media/NOTICE.md`,
      `crates/teamcraft-media/models/NOTICE.md`).
- [ ] **Verification.** Verify the app on Flathub (domain or GitHub organisation) after the merge.
- [ ] **Submission PR** to `flathub/flathub` with the manifest on the `new-pr` branch.

## Native packages

```sh
cd apps/desktop
cargo tauri build --config tauri.models.conf.json --bundles deb,rpm,appimage
```

`tauri.conf.json` declares GStreamer as a dependency of the `.deb` and `.rpm` and recommends the
VA-API plugin (`gstreamer1.0-plugins-bad` / `gstreamer1-plugins-bad-free`) and the tray library
(`libayatana-appindicator3-1` / `libayatana-appindicator-gtk3`). The tray library is only
recommended because the app runs without it, just without a tray icon (as in the Flatpak, whose
runtime doesn't ship it). The release workflow builds them in a Debian 13 container (see the
distribution table for what that implies).

The app only counts a tray library built for its own architecture (a 32-bit one in `/usr/lib`
doesn't), and it only hides the window to the tray (close to tray, start minimized) while a
StatusNotifier host shows the icon: GNOME without the AppIndicator extension has none, and the
window then stays reachable in the taskbar. The app follows the `org.kde.StatusNotifierWatcher` on
the session bus, so a panel that starts later is picked up.
