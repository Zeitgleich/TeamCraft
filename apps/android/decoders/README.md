# Bundled video decoders (Android)

Videos are end-to-end encrypted, so the server can't transcode them (#200); the device decodes what
the sender's device recorded. Android's own decoders come first. For two formats the app brings
its own, used only when no hardware or platform decoder takes the video:

| Module         | Format | Library                 | License | Fills the gap on                                        |
|----------------|--------|-------------------------|---------|---------------------------------------------------------|
| `:decoder-av1` | AV1    | dav1d 1.5.5 (8-bit)     | BSD-2   | Android 8 and 9 (no AV1 decoder), failing ones          |
| `:decoder-vp9` | VP9    | libvpx 1.17.0 (decoder) | BSD-3   | 10-bit VP9 (profile 2), devices whose VP9 decoder fails |

Not bundled:

- **VP8**: every Android device must decode it (Android CDD 5.1.3), and Media3's renderer only
  takes VP9; libvpx is configured with `--disable-vp8`.
- **HEVC**: stays with the hardware decoder (patent licensing per distribution channel).
- **10-bit AV1**: Media3's dav1d wrapper refuses pictures above 8 bits, so dav1d is built 8-bit
  only (`-Dbitdepths=8`); Android 10 and later decode 10-bit AV1 in the platform.
- **GPL code**: none. The Media3 glue is Apache-2.0 ([LICENSE-media3](LICENSE-media3)), dav1d and
  libvpx are BSD; both are compatible with the app's AGPL-3.0.

## How it fits together

- `av1/` and `vp9/` are Media3's `libraries/decoder_av1` and `libraries/decoder_vp9` of the release
  in [media3-version.txt](media3-version.txt) (Java renderers and JNI wrappers), with their own
  Gradle files. Media3 doesn't publish these modules on Google's Maven.
- [build-native.sh](build-native.sh) (run by `scripts/build.sh` before Gradle, inside the
  `teamcraft-android-build` image) downloads the pinned tarballs, checks their SHA-256, builds
  `libdav1d.a` with Meson and libvpx's configuration with its `configure`, into
  `/android/decoders/<hash of the script>`, and links them into the modules' `jni` directories
  (git-ignored). Gradle builds the JNI libraries with the NDK (CMake for AV1, ndk-build for VP9).
  The same image, NDK and pins give the same libraries.
- The app's player (`VideoViewer.kt`) sets `EXTENSION_RENDERER_MODE_ON`: `DefaultRenderersFactory`
  loads the renderers by name and puts them after the platform's. When a decoder fails on an AV1 or
  VP9 video, the player opens it once more with the bundled decoder first
  (`EXTENSION_RENDERER_MODE_PREFER`) and an OpenGL view (`video_player_decoder_gl.xml`): libvpx
  shows 10-bit VP9 only there, and Media3 names it the faster output for these decoders.
- Without `build-native.sh` (an IDE sync) the modules build without their native code and the
  renderers report themselves unavailable; a release build refuses that.

## Changes against Media3

- `av1/src/main/jni/dav1d_jni.cc`: `settings.frame_size_limit = 4096 * 4096` (see "Security").
- `vp9/src/main/jni/Android.mk`: includes `libvpx.mk` by its full path (Gradle runs ndk-build from
  the module directory).
- `build-native.sh` replaces Media3's `build_dav1d.sh` and `generate_libvpx_android_configs.sh`:
  only our ABIs (`arm64-v8a`, `x86_64`), API level 26, plus `--enable-vp9-highbitdepth`,
  `--size-limit=4096x4096` and, on arm64, `--disable-sve --disable-sve2` (`libvpx.mk` has no target
  flags for SVE files) for libvpx.

## Security

The decoders parse video bitstreams from other people in the app's process (the platform's
decoders run in the separate, sandboxed media codec process). So:

- **Limits before allocation**: dav1d refuses frames above 4096 × 4096 pixels, libvpx frames wider
  or taller than 4096; a larger video fails to decode and the viewer offers another app.
- **Only on demand**: the platform decoders come first; the bundled ones parse a stream only when
  the device has none for it, or after the device's failed.
- **Containers stay in Java/Kotlin**: Media3's own extractors (Matroska/WebM, MP4) demux; the C code
  sees codec frames only.
- **Fuzzing**: dav1d and libvpx are fuzzed continuously upstream in OSS-Fuzz (`dav1d_fuzzer`,
  `vpx_dec_fuzzer_vp9`); we add no parser of our own, so no fuzz target here. The JNI wrappers copy
  decoded frames into buffers sized from the decoder's own output.
- **Crashes**: native crashes are reported like the app's own (sentry-android-ndk, full flavor).
- **Updates**: see below. A security release of dav1d or libvpx is applied as a patch release.

## Tests

`src/androidTest` of each module plays its clips with that decoder alone on a device: an 8-bit AV1
clip, VP9 profile 0 and 10-bit profile 2, and a clip above the size limit that must fail. Run them
on a phone (arm64) with the APKs from `:decoder-av1:assembleDebugAndroidTest` and
`:decoder-vp9:assembleDebugAndroidTest` and `adb shell am instrument -w
androidx.media3.decoder.av1.test/androidx.test.runner.AndroidJUnitRunner` (`vp9` likewise). The
clips were made with FFmpeg (`testsrc2` 320x240, 1 s; SVT-AV1 and libvpx-vp9 `-profile:v 2
-pix_fmt yuv420p10le`; 4608x4096 AV1 and 4608x2592 VP9 for the limit).

## Updates

Check for new releases with every app release; bump on any security fix right away.

- **dav1d**: <https://code.videolan.org/videolan/dav1d/-/tags>; the tarball and its `.sha256` are at
  `https://downloads.videolan.org/pub/videolan/dav1d/<version>/`.
- **libvpx**: <https://github.com/webmproject/libvpx/tags>; hash the tag's tarball
  (`curl -fsSL <url> | sha256sum`).
- **cpu_features** (used by the AV1 wrapper to count cores): <https://github.com/google/cpu_features/releases>.

Change the version and hash in `build-native.sh`; the changed script builds afresh.

**Media3**: `gradle/libs.versions.toml`'s `media3` and `media3-version.txt` must be the same minor
release (the modules' Gradle files check it; Dependabot only proposes patch releases). On a minor or
major Media3 bump, copy `src/main/` and `proguard-rules.txt` of
both modules from the new tag, apply the changes listed above again, and compare Media3's
`build_dav1d.sh` and `generate_libvpx_android_configs.sh` with `build-native.sh`.

## Size

Release builds of both modules, libraries stripped as the app packages them (2026-11, dav1d 1.5.5
8-bit, libvpx 1.17.0 VP9 decoder with 10 bit). The APK stores native libraries uncompressed
(`useLegacyPackaging = false`); Play's download is compressed, so the gzip column is close to what a
phone downloads. The App Bundle delivers one ABI per device.

| ABI         | dav1d + JNI | libvpx + JNI | Installed / in the APK | Download (gzip -9) |
|-------------|-------------|--------------|------------------------|--------------------|
| `arm64-v8a` | 0.93 MB     | 0.69 MB      | 1.61 MB                | 0.72 MB            |
| `x86_64`    | 1.43 MB     | 0.63 MB      | 2.06 MB                | 0.81 MB            |

Against the 1.0.41 release: the universal APK (129 MB, both ABIs) grows by 3.7 MB (2.8 %), the
App Bundle (121 MB) by about the same, a phone's Play download by about 0.7 MB. There is no Android
size budget in docs/performance.md; the decoders cost less than 1 % of a phone's download.
