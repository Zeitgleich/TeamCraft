# OCI image of teamcraft-server. Build from the repository root:
#
#   podman build --format docker -f containers/teamcraft-server.Containerfile -t teamcraft-server .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-server healthcheck`.
#
# Base images are pinned by tag and multi-arch index digest so builds are reproducible. Dependabot
# bumps both in the FROM lines; by hand, take the index digest
# (`docker buildx imagetools inspect <image>`, the top-level Digest), never a single-platform one.

FROM docker.io/library/rust:1.99.1-trixie@sha256:931c2c0531a8b143e044f0f3914dfe802f15bb3d1955f0b657e9359327ac2ffc AS build
ENV SQLX_OFFLINE=true \
    CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_STRIP=symbols
WORKDIR /src
COPY . .
# rust-toolchain.toml would make rustup download a toolchain and the wasm target; the image's own
# Rust (pinned by the base image digest) is what we want.
RUN rm rust-toolchain.toml
RUN --mount=type=cache,id=teamcraft-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=teamcraft-server-target,target=/src/target \
    cargo build --release --locked -p teamcraft-server --bin teamcraft-server \
 && install -D target/release/teamcraft-server /out/teamcraft-server

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf
LABEL org.opencontainers.image.title="teamcraft-server" \
      org.opencontainers.image.description="TeamCraft REST API and gateway" \
      org.opencontainers.image.source="https://github.com/Zeitgleich/TeamCraft" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/teamcraft-server /usr/local/bin/teamcraft-server
# The base image already runs as the unprivileged "nonroot" user (65532); say so explicitly so the
# image stays non-root if the base changes. The server writes nothing to disk, so a read-only root
# filesystem works.
USER 65532:65532
# Configuration: a TOML file (--config <path>, TEAMCRAFT_CONFIG) and/or TEAMCRAFT_* variables, see
# teamcraft.example.toml. TLS is terminated by the reverse proxy in front.
ENV TEAMCRAFT_SERVER__LISTEN_ADDR=[::]:8080
EXPOSE 8080/tcp
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/teamcraft-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/teamcraft-server"]
