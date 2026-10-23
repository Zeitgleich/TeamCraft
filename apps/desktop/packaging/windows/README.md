# TeamCraft on Windows

Windows 10 (1809 or newer) and Windows 11, x64 and arm64. The desktop app is the same Tauri 2 shell and
Leptos UI as on Linux and macOS (`apps/desktop`), running `teamcraft-core` natively.

**Status:** everything here was written and compile-checked without a Windows machine (cross-checked with
`cargo xwin`, built and tested by the `windows` CI job on GitHub's runners). Nothing has been run by a
person on a real PC, camera, microphone and GPU yet; see [What needs a real machine](#what-needs-a-real-machine).

## What is used where

| Area                  | Windows implementation                                                                              |
|-----------------------|-----------------------------------------------------------------------------------------------------|
| Webview               | WebView2 (Evergreen runtime; the installers download it if missing, it ships with Windows 11)       |
| Refresh token, DB key | Credential Manager through the `keyring` crate (`windows-native-keyring-store`)                     |
| Local database        | SQLCipher with OpenSSL built from source (`bundled-sqlcipher-vendored-openssl`)                     |
| OIDC login            | System browser plus the loopback listener on `127.0.0.1` (RFC 8252), same as elsewhere              |
| Notifications         | `tauri-plugin-notification` (toast); shown under the app's name once installed                      |
| Links                 | `teamcraft://` registered by the installer; a second launch hands its link to the running instance   |
| Start at login        | `tauri-plugin-autostart` (per-user registry Run entry) behind the shell commands `autostart_get`/`autostart_set` |
| Audio                 | cpal over WASAPI, Opus                                                                              |
| Echo cancellation     | WebRTC audio processing is **not** in the release build yet (see below)                             |
| Cameras               | Media Foundation through `nokhwa` (`mediafoundation` feature)                                       |
| Screen and windows    | Windows Graphics Capture through `windows-capture` (`graphics-capture` feature)                     |
| H.264                 | Cisco's OpenH264 `win64`/`win-arm64` DLL, downloaded on first video use; hardware: see below       |
| Push-to-talk          | `tauri-plugin-global-shortcut` (works with other windows focused)                                   |

## Build

Prerequisites: Rust (MSVC toolchain, `rustup target add wasm32-unknown-unknown`), Visual Studio Build Tools
with the C++ workload (and the ARM64 components for arm64), [Trunk](https://trunkrs.dev), the Tauri CLI
(`cargo install tauri-cli --version "^2" --locked`), [NASM](https://www.nasm.us) and Perl (Strawberry Perl or
Git for Windows' Perl) for building OpenSSL, and Git Bash for `crates/teamcraft-media/models/fetch.sh`.

```powershell
cd apps\desktop
cargo tauri build --bundles nsis,msi                                    # x64, results in target\release\bundle\
cargo tauri build --target aarch64-pc-windows-msvc --bundles nsis,msi   # arm64 (native, or cross on x64)
cargo tauri build --no-bundle                                           # just teamcraft-desktop.exe
```

`tauri.windows.conf.json` is merged automatically on Windows (installer options, the `teamcraft` scheme).
MSI versions must be numeric, so `tauri.windows.conf.json` sets `bundle.windows.wix.version` (a four-part
number); bump it with the app version (a pre-release suffix like `1.0.0-beta.1` is fine for NSIS only).
The NSIS installer installs per user or per machine (the user chooses), in English or German; the MSI
(WiX) is for managed deployments. Both are unsigned.

## Hardware H.264 encoding

The default build encodes in software with OpenH264. Media Foundation's hardware encoders (NVIDIA, AMD,
Intel) are reachable through GStreamer's `mfh264enc`, which is implemented in `video_gstreamer.rs`
behind teamcraft-media's `gstreamer` feature. It needs the GStreamer MSVC runtime on the build machine
(and bundled with, or installed next to, the app), so the feature is off in the installers, as on macOS.
A native Media Foundation encoder behind `VideoEncoder` would remove that dependency but needs a
safe Media Foundation transform wrapper first; none of the maintained crates offers one yet.

## Echo cancellation

Not available on Windows yet, so calls on speakers echo (headsets are fine). WebRTC's audio processing
module builds from source with Meson, but the `webrtc-audio-processing-sys` crate's build script passes
GCC-style flags (`-Wno-unused-parameter`, `-std=c++17`, `nm`) that MSVC's `cl` rejects; an experimental CI
job proved that. Ways forward: patch the build script for MSVC/clang-cl (upstream or a vendored copy), build
with the MinGW target (`x86_64-pc-windows-gnu`), or switch to a pure-Rust echo canceller behind
`AudioProcessor`. The release workflow clears the audio-processing flags for Windows meanwhile.

## Code signing

The installers are unsigned, so SmartScreen warns on first run ("Windows protected your PC"). Options
once there is budget or a registered organization:

1. **Azure Trusted Signing** (cheapest for organizations, no hardware token): set
   `bundle.windows.signCommand` in `tauri.windows.conf.json` to call `trusted-signing-cli`, and add the
   Azure secrets to the workflow.
2. **OV/EV Authenticode certificate** in an HSM or cloud signing service: `signCommand` with the vendor's
   tool (`%1` is the file to sign), or `bundle.windows.certificateThumbprint` with the certificate in the
   runner's store.

Sign both the app executable and the installers; `tauri build` does that when one of these is configured.
Also add `bundle.windows.timestampUrl`. SmartScreen reputation builds up per certificate (EV certificates
skip the warm-up).

## winget

`winget/` holds a draft manifest in the three-file layout `microsoft/winget-pkgs` expects. At release time:
replace `0.0.0` with the version, point `InstallerUrl` at the release assets, fill in `InstallerSha256` (the
`sha256sum` of each NSIS installer), run `winget validate --manifest apps/desktop/packaging/windows/winget`
and `winget install --manifest ...` on Windows, then open a pull request against winget-pkgs (or use
`wingetcreate`). It needs a signed installer to pass the store's SmartScreen check comfortably.

## What needs a real machine

Compile-checked or covered by unit tests only:

- WebView2 start, window behavior, installers (NSIS and MSI) and uninstaller, the WebView2 bootstrapper.
- Credential Manager round trip, SQLCipher on the vendored OpenSSL (CI runs the unit tests, which open an
  encrypted database).
- Camera enumeration and capture through Media Foundation (nokhwa's formats on real devices, MJPEG vs NV12).
- Screen and window capture (frame pacing, size changes, the capture border on Windows 10, protected
  windows). The source is chosen by token (`monitor:N`, `window:Title`, default: primary display); there is
  no in-app picker yet. The system picker (`GraphicsCapturePicker`) would need a way to stop a capture
  whose item lives on the picker's thread.
- WASAPI audio devices and exclusive-mode apps, the push-to-talk global shortcut, toast notifications,
  `teamcraft://` links, second-instance hand-over, autostart.
- arm64 builds (built natively on the `windows-11-arm` runner; the OpenH264 `win-arm64` hash is in the
  `openh264` crate).
