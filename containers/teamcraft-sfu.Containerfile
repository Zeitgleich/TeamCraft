# OCI image of teamcraft-sfu. Build from the repository root:
#
#   podman build --format docker -f containers/teamcraft-sfu.Containerfile -t teamcraft-sfu .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-sfu healthcheck`.
#
# Base images are pinned by digest so builds are reproducible; bump them deliberately
# with the multi-arch index digest
# (`docker buildx imagetools inspect <image>`, the top-level Digest), never a single-platform one.

ARG RUST_IMAGE=docker.io/library/rust:1.99.1-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

FROM ${RUST_IMAGE} AS build
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

FROM ${RUNTIME_IMAGE}
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
