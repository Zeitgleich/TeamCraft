# OCI image of teamcraft-sfu. Build from the repository root:
#
#   podman build --format docker -f containers/teamcraft-sfu.Containerfile -t teamcraft-sfu .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-sfu healthcheck`.
#
# Base images are pinned by tag and multi-arch index digest so builds are reproducible. Dependabot
# bumps both in the FROM lines; by hand, take the index digest
# (`docker buildx imagetools inspect <image>`, the top-level Digest), never a single-platform one.

FROM docker.io/library/rust:1.99.1-trixie@sha256:931c2c0531a8b143e044f0f3914dfe802f15bb3d1955f0b657e9359327ac2ffc AS build
ENV CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_STRIP=symbols
WORKDIR /src
COPY . .
# rust-toolchain.toml would make rustup download a toolchain and the wasm target; the image's own
# Rust (pinned by the base image digest) is what we want.
RUN rm rust-toolchain.toml
RUN --mount=type=cache,id=teamcraft-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=teamcraft-sfu-target,target=/src/target \
    cargo build --release --locked -p teamcraft-sfu --bin teamcraft-sfu \
 && install -D target/release/teamcraft-sfu /out/teamcraft-sfu

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e
LABEL org.opencontainers.image.title="teamcraft-sfu" \
      org.opencontainers.image.description="TeamCraft WebTransport SFU for calls" \
      org.opencontainers.image.source="https://github.com/Zeitgleich/TeamCraft" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/teamcraft-sfu /usr/local/bin/teamcraft-sfu
# Non-root, and the SFU writes nothing to disk, so a read-only root filesystem works. Without
# TEAMCRAFT_SFU_CERT / TEAMCRAFT_SFU_KEY it generates a short-lived self-signed certificate in memory;
# production mounts a real certificate and sets TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY and
# TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER (see crates/teamcraft-sfu/src/main.rs).
USER 65532:65532
ENV TEAMCRAFT_SFU_BIND=[::]:4433 \
    TEAMCRAFT_SFU_HEALTH_BIND=[::]:8081
EXPOSE 4433/udp 8081/tcp
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/teamcraft-sfu", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/teamcraft-sfu"]
