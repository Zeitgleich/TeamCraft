# Runs wasm-bindgen-test suites in headless Chromium (crates/teamcraft-core/README.md, "Browser tests").
FROM rust:1.99.1-trixie

RUN apt-get update \
  && apt-get install -y --no-install-recommends chromium chromium-driver \
  && rm -rf /var/lib/apt/lists/*
# The image's Rust is what rust-toolchain.toml's "stable" means today; use it instead of downloading.
ENV RUSTUP_TOOLCHAIN=1.99.1
RUN rustup target add wasm32-unknown-unknown
# Must match the wasm-bindgen version in Cargo.lock exactly.
ARG WASM_BINDGEN_VERSION=0.2.129
RUN cargo install --locked wasm-bindgen-cli --version "${WASM_BINDGEN_VERSION}" \
  && rm -rf /usr/local/cargo/registry

ENV CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
  CHROMEDRIVER=/usr/bin/chromedriver
