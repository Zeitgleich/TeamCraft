#!/usr/bin/env bash
# Fetches and builds the native decoder libraries for the vendored Media3 decoder modules
# (README.md): dav1d (AV1) and libvpx (VP9). Runs inside the teamcraft-android-build image from
# scripts/build.sh, before Gradle; Gradle builds the JNI wrappers and links them.
#
# Sources are pinned release tarballs checked against SHA-256. The result goes to a directory named
# after this script's hash in the cache (TEAMCRAFT_DECODERS_CACHE, default /android/decoders), so a
# changed pin or flag builds afresh and an unchanged one is reused. The modules' jni directories get
# symlinks into it (git-ignored).
#
#   decoders/build-native.sh            needs ANDROID_NDK_HOME
set -euo pipefail

# Keep these current: every release of dav1d and libvpx can fix parser bugs (README.md, "Updates").
DAV1D_VERSION=1.5.5
DAV1D_SHA256=ca96b1d0ba3f01f55de7231d775892b64994ba061aaeeaaa4f662a390b3abf9e
LIBVPX_VERSION=1.17.0
LIBVPX_SHA256=1020f184046187baa2985dbde38e0691f49c44088bca7a1842b0236c6081dc0a
CPU_FEATURES_VERSION=0.11.0
CPU_FEATURES_SHA256=ab2463f2d38fcaff1ce806be8e4c91333449931f5e02009d543b2569a3fa471a

# The app's ABIs (app/build.gradle.kts, abiFilters) and its minSdk.
ABIS=(arm64-v8a x86_64)
API_LEVEL=26
# The largest frame libvpx decodes, per dimension; dav1d's limit is in its JNI wrapper. Videos come
# from other people and are decoded in the app's process (README.md, "Security").
VPX_SIZE_LIMIT=4096x4096

DECODERS_DIR="$(cd "$(dirname "$0")" && pwd)"
CACHE_ROOT="${TEAMCRAFT_DECODERS_CACHE:-/android/decoders}"
NDK="${ANDROID_NDK_HOME:?ANDROID_NDK_HOME must name the NDK}"
TOOLCHAIN="${NDK}/toolchains/llvm/prebuilt/linux-x86_64/bin"

main() {
  local stamp out
  stamp="$( (cat "$0"; basename "${NDK}") | sha256sum | cut -c1-16)"
  out="${CACHE_ROOT}/${stamp}"
  if [ ! -f "${out}/done" ]; then
    rm -rf "${out}"
    mkdir -p "${out}/downloads"
    fetch "${out}" "dav1d-${DAV1D_VERSION}.tar.xz" \
      "https://downloads.videolan.org/pub/videolan/dav1d/${DAV1D_VERSION}/dav1d-${DAV1D_VERSION}.tar.xz" "${DAV1D_SHA256}"
    fetch "${out}" "libvpx-${LIBVPX_VERSION}.tar.gz" \
      "https://github.com/webmproject/libvpx/archive/refs/tags/v${LIBVPX_VERSION}.tar.gz" "${LIBVPX_SHA256}"
    fetch "${out}" "cpu_features-${CPU_FEATURES_VERSION}.tar.gz" \
      "https://github.com/google/cpu_features/archive/refs/tags/v${CPU_FEATURES_VERSION}.tar.gz" "${CPU_FEATURES_SHA256}"
    build_dav1d "${out}"
    configure_libvpx "${out}"
    rm -rf "${out}/downloads"
    touch "${out}/done"
  fi
  link_into_modules "${out}"
  # Older builds of the decoders: only the current one is used.
  find "${CACHE_ROOT}" -mindepth 1 -maxdepth 1 -type d ! -name "${stamp}" -exec rm -rf {} +
}

# Downloads a tarball, checks its hash and unpacks it into <out>/<name without extension>.
fetch() {
  local out="$1" name="$2" url="$3" sha256="$4" file
  file="${out}/downloads/${name}"
  curl -fsSL --retry 3 -o "${file}" "${url}"
  echo "${sha256}  ${file}" | sha256sum -c --quiet -
  mkdir -p "${out}/${name%%.tar.*}"
  tar -xf "${file}" -C "${out}/${name%%.tar.*}" --strip-components=1
}

# Static libdav1d per ABI, as Media3's build_dav1d.sh does, without tools or tests, and 8-bit only:
# Media3's JNI wrapper refuses pictures of more than 8 bits (10-bit AV1 stays with the platform).
build_dav1d() {
  local out="$1" src="$1/dav1d-${DAV1D_VERSION}" abi triple cross build
  for abi in "${ABIS[@]}"; do
    case "${abi}" in
      arm64-v8a) triple=aarch64-linux-android; cross=aarch64-android.meson ;;
      x86_64) triple=x86_64-linux-android; cross=x86_64-android.meson ;;
    esac
    build="${out}/dav1d-build/${abi}"
    mkdir -p "${build}"
    sed -e "s|^c = .*|c = '${TOOLCHAIN}/${triple}${API_LEVEL}-clang'|" \
      -e "s|^cpp = .*|cpp = '${TOOLCHAIN}/${triple}${API_LEVEL}-clang++'|" \
      -e "s|^ar = .*|ar = '${TOOLCHAIN}/llvm-ar'|" \
      -e "s|^strip = .*|strip = '${TOOLCHAIN}/llvm-strip'|" \
      "${src}/package/crossfiles/${cross}" >"${build}/cross.meson"
    meson setup "${build}" "${src}" --cross-file="${build}/cross.meson" \
      --buildtype=release --default-library=static -Db_ndebug=true -Dbitdepths=8 \
      -Denable_tools=false -Denable_tests=false -Denable_examples=false \
      -Dc_args="-ffile-prefix-map=${out}=."
    ninja -C "${build}" src/libdav1d.a
    mkdir -p "${out}/nativelib/${abi}"
    cp "${build}/src/libdav1d.a" "${out}/nativelib/${abi}/"
  done
  rm -rf "${out}/dav1d-build"
}

# libvpx's configuration headers and source lists per ABI, for ndk-build (vp9/src/main/jni/libvpx.mk).
# Media3's generate_libvpx_android_configs.sh for our ABIs, plus 10-bit VP9 (profile 2, the VP9 the
# platform decoders may lack) and the frame size limit. VP8 is left out: every Android device must
# decode it (Android CDD 5.1.3), and Media3's renderer only takes VP9.
configure_libvpx() {
  local out="$1" src="$1/libvpx-${LIBVPX_VERSION}" abi config f
  local common=(--disable-examples --disable-docs --disable-tools --disable-unit-tests
    --enable-realtime-only --disable-vp8 --disable-vp9-encoder --disable-webm-io --disable-libyuv
    --enable-external-build --enable-vp9-highbitdepth --size-limit="${VPX_SIZE_LIMIT}")
  local allowed=(libvpx_srcs.txt vpx_config.c vpx_config.h vpx_scale_rtcd.h vp8_rtcd.h vp9_rtcd.h
    vpx_version.h vpx_config.asm vpx_dsp_rtcd.h libvpx.ver)
  for abi in "${ABIS[@]}"; do
    case "${abi}" in
      # SVE code needs per-file target flags Media3's libvpx.mk only sets for dotprod and i8mm.
      arm64-v8a) config=(--force-target=armv8-android-gcc --enable-neon --enable-runtime-cpu-detect
        --disable-sve --disable-sve2) ;;
      # The emulator and x86 Chromebooks: plain C, as Media3 configures it (no assembler needed).
      x86_64) config=(--force-target=x86_64-android-gcc --disable-mmx --disable-sse --disable-sse2 --disable-sse3 --disable-ssse3
        --disable-sse4_1 --disable-avx --disable-avx2 --disable-avx512 --enable-pic --disable-neon
        --disable-neon-asm --disable-runtime-cpu-detect) ;;
    esac
    mkdir -p "${out}/libvpx_android_configs/${abi}"
    (
      cd "${out}/libvpx_android_configs/${abi}"
      "${src}/configure" "${config[@]}" "${common[@]}"
      rm -f libvpx_srcs.txt
      # Not every file exists in every configuration (vp8_rtcd.h without VP8).
      for f in "${allowed[@]}"; do make "${f}" >/dev/null 2>&1 || true; done
      find . -mindepth 1 -maxdepth 1 $(printf '! -name %s ' "${allowed[@]}") -exec rm -rf {} +
    )
    [ -s "${out}/libvpx_android_configs/${abi}/libvpx_srcs.txt" ] \
      || { echo "error: libvpx configure produced no source list for ${abi}" >&2; exit 1; }
  done
  mv "${src}" "${out}/libvpx"
}

link_into_modules() {
  local out="$1"
  ln -sfn "${out}/dav1d-${DAV1D_VERSION}" "${DECODERS_DIR}/av1/src/main/jni/dav1d"
  ln -sfn "${out}/cpu_features-${CPU_FEATURES_VERSION}" "${DECODERS_DIR}/av1/src/main/jni/cpu_features"
  ln -sfn "${out}/nativelib" "${DECODERS_DIR}/av1/src/main/jni/nativelib"
  ln -sfn "${out}/libvpx" "${DECODERS_DIR}/vp9/src/main/jni/libvpx"
  ln -sfn "${out}/libvpx_android_configs" "${DECODERS_DIR}/vp9/src/main/jni/libvpx_android_configs"
}

main "$@"
