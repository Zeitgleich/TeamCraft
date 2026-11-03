# OCI image of teamcraft-notetaker (docs/e2ee.md §8.2, docs/admin.md §3.7). Build from the
# repository root:
#
#   podman build --format docker -f containers/teamcraft-notetaker.Containerfile -t teamcraft-notetaker .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-notetaker healthcheck`.
#
# Base images are pinned by tag and multi-arch index digest so builds are reproducible. Dependabot
# bumps both in the FROM lines; by hand, take the index digest
# (`docker buildx imagetools inspect <image>`, the top-level Digest), never a single-platform one.
#
# Like the bridge, the encrypted local database (SQLCipher) links the system libcrypto, which
# distroless "cc" ships. The call driver (teamcraft-media `native`) builds libopus with CMake and
# links ALSA's libasound through cpal; the notetaker opens no audio device, but the library must be
# there for the binary to start, so the build stage copies it into the image.

FROM docker.io/library/rust:1.99.1-trixie@sha256:931c2c0531a8b143e044f0f3914dfe802f15bb3d1955f0b657e9359327ac2ffc AS build
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
    --mount=type=cache,id=teamcraft-notetaker-target,target=/src/target \
    cargo build --release --locked -p teamcraft-notetaker --bin teamcraft-notetaker \
 && install -D target/release/teamcraft-notetaker /out/teamcraft-notetaker \
 && multiarch="$(gcc -print-multiarch)" \
 && install -D -m 0644 "$(readlink -f "/usr/lib/${multiarch}/libasound.so.2")" "/out/lib/${multiarch}/libasound.so.2" \
 && mkdir -p /out/data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf
LABEL org.opencontainers.image.title="teamcraft-notetaker" \
      org.opencontainers.image.description="TeamCraft notetaker: a call participant that transcribes calls through OpenRouter and posts a summary" \
      org.opencontainers.image.source="https://github.com/Zeitgleich/TeamCraft" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
# libasound into the platform's library directory (/usr/lib/<multiarch>), where the loader looks.
COPY --from=build /out/lib/ /usr/lib/
COPY --from=build /out/teamcraft-notetaker /usr/local/bin/teamcraft-notetaker
COPY --from=build --chown=65532:65532 /out/data /var/lib/teamcraft-notetaker
# Non-root. The notetaker keeps its database (the bot's keys, its conversations, the transcripts of
# calls in progress) and its liveness file in the volume at /var/lib/teamcraft-notetaker; call audio
# never touches the disk. It listens on no port: it connects out to the server, the SFU (QUIC/UDP)
# and OpenRouter. The rest of the filesystem can be read-only.
USER 65532:65532
ENV TEAMCRAFT_NOTETAKER_STORE__PATH=/var/lib/teamcraft-notetaker/notetaker.db
VOLUME ["/var/lib/teamcraft-notetaker"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
  CMD ["/usr/local/bin/teamcraft-notetaker", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/teamcraft-notetaker"]
