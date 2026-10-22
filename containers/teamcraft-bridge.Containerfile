# OCI image of teamcraft-bridge. Build from the repository root:
#
#   podman build --format docker -f containers/teamcraft-bridge.Containerfile -t teamcraft-bridge .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-bridge healthcheck`.
#
# Base images are pinned by digest so builds are reproducible; bump them deliberately
# (`podman pull <image>` and copy the new digest).
#
# Unlike the server images this one needs distroless "base" instead of "cc": the encrypted local
# database (SQLCipher) links the system libcrypto, which "base" ships (with the CA certificates).
# "base" lacks libgcc_s, which the Rust runtime needs, so the build stage hands it over.

ARG RUST_IMAGE=docker.io/library/rust:1-bookworm@sha256:8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8
ARG RUNTIME_IMAGE=gcr.io/distroless/base-debian12:nonroot@sha256:5883c320d81f76c764e114c9ab14054f037bb040907816fc9028d80d9602a7c3

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
