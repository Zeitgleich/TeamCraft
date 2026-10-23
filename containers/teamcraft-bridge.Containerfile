# OCI image of teamcraft-bridge. Build from the repository root:
#
#   podman build --format docker -f containers/teamcraft-bridge.Containerfile -t teamcraft-bridge .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-bridge healthcheck`.
#
# Base images are pinned by tag and multi-arch index digest so builds are reproducible. Dependabot
# bumps both in the FROM lines; by hand, take the index digest
# (`docker buildx imagetools inspect <image>`, the top-level Digest), never a single-platform one.
#
# The encrypted local database (SQLCipher) links the system libcrypto; distroless "cc" ships it
# (OpenSSL 3 from the same Debian release as the build stage) along with libgcc_s.

FROM docker.io/library/rust:1.99.1-trixie@sha256:931c2c0531a8b143e044f0f3914dfe802f15bb3d1955f0b657e9359327ac2ffc AS build
ENV CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_STRIP=symbols
WORKDIR /src
COPY . .
# rust-toolchain.toml would make rustup download a toolchain and the wasm target; the image's own
# Rust (pinned by the base image digest) is what we want.
RUN rm rust-toolchain.toml
RUN --mount=type=cache,id=teamcraft-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=teamcraft-bridge-target,target=/src/target \
    cargo build --release --locked -p teamcraft-bridge --bin teamcraft-bridge \
 && install -D target/release/teamcraft-bridge /out/teamcraft-bridge \
 && mkdir -p /out/data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf
LABEL org.opencontainers.image.title="teamcraft-bridge" \
      org.opencontainers.image.description="TeamCraft integration bridge: webhooks and logs into encrypted channels" \
      org.opencontainers.image.source="https://github.com/Zeitgleich/TeamCraft" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/teamcraft-bridge /usr/local/bin/teamcraft-bridge
COPY --from=build --chown=65532:65532 /out/data /var/lib/teamcraft-bridge
# Non-root. The bridge writes only its database, in the volume at /var/lib/teamcraft-bridge (owned
# by 65532); the rest of the filesystem can be read-only.
USER 65532:65532
# Inside a container the endpoints must listen on all interfaces (publish the port on loopback
# only on the host: `-p 127.0.0.1:8088:8088`). `[::]` is dual stack.
ENV TEAMCRAFT_BRIDGE_LISTEN__ADDRS='["[::]:8088"]' \
    TEAMCRAFT_BRIDGE_STORE__PATH=/var/lib/teamcraft-bridge/bridge.db
VOLUME ["/var/lib/teamcraft-bridge"]
EXPOSE 8088/tcp
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD ["/usr/local/bin/teamcraft-bridge", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/teamcraft-bridge"]
