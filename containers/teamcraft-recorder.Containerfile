# OCI image of teamcraft-recorder, the recorder bot (docs/e2ee.md §8.3, docs/admin.md §3.13,
# docs/operations.md "Recorder bot"). Build from the repository root:
#
#   podman build --format docker -f containers/teamcraft-recorder.Containerfile -t teamcraft-recorder .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-recorder healthcheck`.
#
# Base images are pinned by tag and multi-arch index digest so builds are reproducible. Dependabot
# bumps both in the FROM lines; by hand, take the index digest
# (`docker buildx imagetools inspect <image>`, the top-level Digest), never a single-platform one.
#
# Like the bridge, the encrypted local database (SQLCipher) links the system libcrypto, which
# distroless "cc" ships. The call driver (teamcraft-media `native`) builds libopus with CMake and
# links ALSA's libasound through cpal; the recorder opens no audio device, but the library must be
# there for the binary to start, so the build stage copies it into the image.

FROM docker.io/library/rust:1.100.0-trixie@sha256:223cc32ddca8f3ee90c436dbc94b6b5f052ea7f9653c5201e9612f08ad7d21b8 AS build
ENV CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_STRIP=symbols
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake libasound2-dev pkgconf \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# rust-toolchain.toml would make rustup download a toolchain and the wasm target; the image's own
# Rust (pinned by the base image digest) is what we want.
RUN rm rust-toolchain.toml
RUN --mount=type=cache,id=teamcraft-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=teamcraft-recorder-target,target=/src/target \
    cargo build --release --locked -p teamcraft-recorder --bin teamcraft-recorder \
 && install -D target/release/teamcraft-recorder /out/teamcraft-recorder \
 && multiarch="$(gcc -print-multiarch)" \
 && install -D -m 0644 "$(readlink -f "/usr/lib/${multiarch}/libasound.so.2")" "/out/lib/${multiarch}/libasound.so.2" \
 && mkdir -p /out/data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:532c7a94f9e96ba9db75eb9388f4227751131c7cb0a411d1a8521af4096135de
LABEL org.opencontainers.image.title="teamcraft-recorder" \
      org.opencontainers.image.description="TeamCraft recorder bot: a call participant that records the calls of its conversations and posts the recordings, encrypted" \
      org.opencontainers.image.source="https://github.com/Zeitgleich/TeamCraft" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
# libasound into the platform's library directory (/usr/lib/<multiarch>), where the loader looks.
COPY --from=build /out/lib/ /usr/lib/
COPY --from=build /out/teamcraft-recorder /usr/local/bin/teamcraft-recorder
COPY --from=build --chown=65532:65532 /out/data /var/lib/teamcraft-recorder
# Non-root. The recorder keeps its database (the bot's keys, its conversations), the sealed takes of
# calls being recorded, the decrypted copies waiting for their upload and its liveness file in the
# volume at /var/lib/teamcraft-recorder. It listens on no port: it connects out to the server and
# the SFU (QUIC/UDP). The rest of the filesystem can be read-only.
USER 65532:65532
ENV TEAMCRAFT_RECORDER_STORE__PATH=/var/lib/teamcraft-recorder/recorder.db
VOLUME ["/var/lib/teamcraft-recorder"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
  CMD ["/usr/local/bin/teamcraft-recorder", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/teamcraft-recorder"]
