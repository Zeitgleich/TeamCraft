# OCI image of teamcraft-push-gateway. Build from the repository root:
#
#   podman build --format docker -f containers/teamcraft-push-gateway.Containerfile -t teamcraft-push-gateway .
#
# `--format docker` keeps the HEALTHCHECK instruction (the OCI image format has none); orchestrators
# that run the image should configure the same command themselves: `teamcraft-push-gateway healthcheck`.
#
# Base images are pinned by digest so builds are reproducible; bump them deliberately
# (`podman pull <image>` and copy the new digest).

ARG RUST_IMAGE=docker.io/library/rust:1-bookworm@sha256:8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot@sha256:777e96cf322c46bc32aca926c263624c4dc8d7cf37e2fa65ba2c7e697318ebbb

FROM ${RUST_IMAGE} AS build
ENV CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_STRIP=symbols
WORKDIR /src
COPY . .
# rust-toolchain.toml would make rustup download a toolchain and the wasm target; the image's own
# Rust (pinned by the base image digest) is what we want.
RUN rm rust-toolchain.toml
RUN --mount=type=cache,id=teamcraft-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=teamcraft-push-gateway-target,target=/src/target \
    cargo build --release --locked -p teamcraft-push-gateway --bin teamcraft-push-gateway \
 && install -D target/release/teamcraft-push-gateway /out/teamcraft-push-gateway

FROM ${RUNTIME_IMAGE}
LABEL org.opencontainers.image.title="teamcraft-push-gateway" \
      org.opencontainers.image.description="TeamCraft push gateway: forwards sealed wake-ups to APNs and FCM" \
      org.opencontainers.image.source="https://github.com/Zeitgleich/TeamCraft" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/teamcraft-push-gateway /usr/local/bin/teamcraft-push-gateway
# Non-root. The only state is the SQLite database: mount a volume at /var/lib/teamcraft-push-gateway
# writable for UID 65532 (the root filesystem can stay read-only). Credentials come as mounted files
# or TEAMCRAFT_PUSH_GATEWAY__APNS__* / __FCM__* variables; see
# crates/teamcraft-push-gateway/teamcraft-push-gateway.example.toml. Port 8091 (health, metrics) is for
# the operator's network only: do not publish it.
USER 65532:65532
ENV TEAMCRAFT_PUSH_GATEWAY__SERVER__BIND=[::]:8090 \
    TEAMCRAFT_PUSH_GATEWAY__SERVER__HEALTH_BIND=[::]:8091 \
    TEAMCRAFT_PUSH_GATEWAY__DATABASE__PATH=/var/lib/teamcraft-push-gateway/gateway.db
VOLUME /var/lib/teamcraft-push-gateway
EXPOSE 8090/tcp
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/teamcraft-push-gateway", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/teamcraft-push-gateway"]
