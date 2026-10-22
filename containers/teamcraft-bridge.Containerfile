# OCI image of teamcraft-bridge. Build from the repository root:
#
#   podman build --format docker -f containers/teamcraft-bridge.Containerfile -t teamcraft-bridge .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-bridge healthcheck`.
#
# Base images are pinned by digest so builds are reproducible; bump them deliberately
# with the multi-arch index digest
# (`docker buildx imagetools inspect <image>`, the top-level Digest), never a single-platform one.
#
# Unlike the server images this one needs distroless "base" instead of "cc": the encrypted local
# database (SQLCipher) links the system libcrypto, which "base" ships (with the CA certificates).
# "base" lacks libgcc_s, which the Rust runtime needs, so the build stage hands it over.

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
    --mount=type=cache,id=teamcraft-bridge-target,target=/src/target \
    cargo build --release --locked -p teamcraft-bridge --bin teamcraft-bridge \
 && install -D target/release/teamcraft-bridge /out/teamcraft-bridge \
 && mkdir -p /out/data /out/lib \
 && cp "$(gcc -print-file-name=libgcc_s.so.1)" /out/lib/

FROM ${RUNTIME_IMAGE}
LABEL org.opencontainers.image.title="teamcraft-bridge" \
      org.opencontainers.image.description="TeamCraft integration bridge: webhooks and logs into encrypted channels" \
      org.opencontainers.image.source="https://github.com/Zeitgleich/TeamCraft" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/teamcraft-bridge /usr/local/bin/teamcraft-bridge
COPY --from=build /out/lib/libgcc_s.so.1 /usr/local/lib/libgcc_s.so.1
COPY --from=build --chown=65532:65532 /out/data /var/lib/teamcraft-bridge
# Non-root. The bridge writes only its database, in the volume at /var/lib/teamcraft-bridge (owned
# by 65532); the rest of the filesystem can be read-only.
USER 65532:65532
# Inside a container the endpoints must listen on all interfaces (publish the port on loopback
# only on the host: `-p 127.0.0.1:8088:8088`). `[::]` is dual stack.
ENV LD_LIBRARY_PATH=/usr/local/lib \
    TEAMCRAFT_BRIDGE_LISTEN__ADDRS='["[::]:8088"]' \
    TEAMCRAFT_BRIDGE_STORE__PATH=/var/lib/teamcraft-bridge/bridge.db
VOLUME ["/var/lib/teamcraft-bridge"]
EXPOSE 8088/tcp
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD ["/usr/local/bin/teamcraft-bridge", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/teamcraft-bridge"]
