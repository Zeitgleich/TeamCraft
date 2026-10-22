# Android build image for apps/android (README.md, "Build"). Kept small on purpose: the SDK
# packages, the NDK and the Gradle and Cargo caches live in a mounted cache directory
# (/android), installed on first use by scripts/build.sh.
FROM rust:1.99.1-slim-trixie

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    openjdk-21-jdk-headless unzip curl ca-certificates git cmake perl python3 \
    # Libraries the emulator loads even when headless (scripts/emulator-test.sh).
    libpulse0 libnss3 libxcomposite1 libxcursor1 libxdamage1 libxi6 libxtst6 libxrandr2 \
    libgl1 libegl1 libdbus-1-3 libasound2t64 libxkbfile1 libxkbcommon0 \
  && rm -rf /var/lib/apt/lists/*

# The image's Rust is what rust-toolchain.toml's "stable" means today; use it instead of downloading.
ENV RUSTUP_TOOLCHAIN=1.99.1
RUN rustup target add aarch64-linux-android x86_64-linux-android
RUN cargo install --locked cargo-ndk --version 4.1.2 && rm -rf /usr/local/cargo/registry

# Android command-line tools (sdkmanager); everything else is installed into the cache.
ARG CMDLINE_TOOLS_BUILD=11076708
RUN curl -fsSL -o /tmp/cmdline-tools.zip \
    "https://dl.google.com/android/repository/commandlinetools-linux-${CMDLINE_TOOLS_BUILD}_latest.zip" \
  && mkdir -p /opt/android-cmdline-tools \
  && unzip -q /tmp/cmdline-tools.zip -d /opt/android-cmdline-tools \
  && rm /tmp/cmdline-tools.zip

ENV ANDROID_HOME=/android/sdk \
  ANDROID_SDK_ROOT=/android/sdk \
  GRADLE_USER_HOME=/android/gradle \
  CARGO_TARGET_DIR=/android/target \
  PATH="/opt/android-cmdline-tools/cmdline-tools/bin:${PATH}"
