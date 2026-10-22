# Build image of the CI jobs: Debian Trixie with Rust, every system package the workspace needs
# (desktop shell, native call media, portal capture, software Vulkan for the effects' golden
# tests, the bundled audio processing build) and the cargo tools the jobs use. Jobs run inside it
# instead of installing packages and tools on every run.
#
# Built and pushed for amd64 and arm64 by .github/workflows/ci-image.yml whenever this file or the
# toolchain pin changes; the workflows pin the pushed image by digest. Locally:
#
#   podman build -f containers/ci.Containerfile -t teamcraft-ci .
#
# The base image is pinned by digest (the multi-arch index of rust:1.99.1-trixie); bump it
# deliberately: `podman pull <image>` and copy the new digest.

ARG RUST_IMAGE=docker.io/library/rust:1.99.1-trixie@sha256:931c2c0531a8b143e044f0f3914dfe802f15bb3d1955f0b657e9359327ac2ffc
FROM ${RUST_IMAGE}

# Packages, grouped by what needs them:
#  - cmake, libclang-dev, pkg-config: codec and bindgen builds (opusic-sys, pipewire bindings)
#  - libwebkit2gtk, gtk, soup, rsvg, ayatana-appindicator, patchelf, rpm, file, libfuse2t64: the
#    Tauri desktop shell and its deb/rpm/AppImage bundles
#  - libasound2, libpipewire, libspa: native audio and portal capture
#  - gstreamer: camera capture
#  - meson, ninja: the bundled WebRTC audio processing build
#  - mesa-vulkan-drivers: lavapipe, a software GPU, so the video effects' golden-image tests run
#  - git, curl, ca-certificates, xz-utils: checkout, downloads (segmentation model), cache actions
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl file git xz-utils pkg-config cmake meson ninja-build \
      libclang-dev \
      libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev libayatana-appindicator3-dev \
      patchelf rpm libfuse2t64 \
      libasound2-dev libpipewire-0.3-dev libspa-0.2-dev \
      libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-bad \
      mesa-vulkan-drivers \
 && rm -rf /var/lib/apt/lists/*

# rust-toolchain.toml asks for `stable` plus the wasm target. RUSTUP_TOOLCHAIN overrides it with the
# image's pinned Rust, which is the stable the project builds with, so rustup never downloads
# another toolchain inside a job. The components are added here once.
ENV RUSTUP_TOOLCHAIN=1.99.1
RUN rustup component add rustfmt clippy \
 && rustup target add wasm32-unknown-unknown

# Cargo tools, built once here and not in every job. Versions are pinned and `--locked`.
ARG NEXTEST_VERSION=0.9.149
ARG SCCACHE_VERSION=0.18.0
ARG TRUNK_VERSION=0.22.0
ARG TAURI_CLI_VERSION=2.12.4
RUN cargo install --locked cargo-nextest@${NEXTEST_VERSION} sccache@${SCCACHE_VERSION} \
      trunk@${TRUNK_VERSION} tauri-cli@${TAURI_CLI_VERSION} \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git

# The jobs set RUSTC_WRAPPER themselves (only where the cache backend is configured).
ENV CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0

LABEL org.opencontainers.image.title="teamcraft-ci" \
      org.opencontainers.image.description="Build and test image of the TeamCraft CI jobs" \
      org.opencontainers.image.source="https://github.com/zeitgleich/teamcraft"
