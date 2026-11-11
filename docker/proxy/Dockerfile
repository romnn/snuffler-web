# syntax=docker/dockerfile:1

# snuffler-web-proxy: the allowlisted CORS + WebSocket-to-TCP proxy the app talks
# to for non-CORS FDSN endpoints and for SeedLink/ringserver streams.
# Same binary micromux runs locally on 127.0.0.1:17684.
#
# Caching is cargo-chef's: the dependency graph is compiled against a skeleton
# workspace in a layer keyed only on Cargo.toml/Cargo.lock, so editing the proxy
# recompiles the proxy and nothing else.
# The cargo registry is left in the layer rather than on a cache mount on purpose
# — a cache mount is invisible to registry/GHA layer caching, so in CI it would
# start empty on every run and the restored layers would re-download the whole
# index.
#
#   docker build -f docker/proxy/Dockerfile -t snuffler-proxy .
#
# CARGO_PROFILE picks the Cargo profile.
# Release images use the default; CI builds every untagged commit with `dev`,
# which compiles much faster and is still enough for the smoke test.
#
#   docker build -f docker/proxy/Dockerfile --build-arg CARGO_PROFILE=dev .

ARG RUST_IMAGE=rust:1-trixie
ARG RUNTIME_IMAGE=debian:trixie-slim
ARG CARGO_CHEF_VERSION=0.1.78

FROM ${RUST_IMAGE} AS toolchain

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_TERM_COLOR=never

# cmake builds aws-lc-sys, the C library behind reqwest's rustls backend.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# rustup materializes rust-toolchain.toml on any proxied command, so this both
# installs and pins the workspace's toolchain in a layer of its own.
COPY rust-toolchain.toml ./
RUN rustup show

# Built away from rust-toolchain.toml so it compiles under the image's own stable
# toolchain rather than dragging the workspace's pinned nightly into a tool that
# has nothing to do with it.
ARG CARGO_CHEF_VERSION
WORKDIR /opt/chef
RUN cargo install cargo-chef --locked --version ${CARGO_CHEF_VERSION}
WORKDIR /src

FROM toolchain AS planner
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
RUN cargo chef prepare --recipe-path recipe.json

FROM toolchain AS builder
ARG CARGO_PROFILE=release

# `-p` matters: the recipe describes the whole workspace, including the wasm-only
# frontend whose native dependency set (eframe, x11) this image has no reason to
# build.
COPY .cargo .cargo
COPY --from=planner /src/recipe.json recipe.json
# The cook compiles the workspace crates as placeholder sources that import
# nothing, so Cargo would report every one of their dependencies as unused; the
# real build below reports any genuine warning.
RUN CARGO_BUILD_WARNINGS=allow \
    cargo chef cook --profile "${CARGO_PROFILE}" --recipe-path recipe.json -p snuffler-web-proxy

COPY Cargo.toml Cargo.lock ./
COPY crates crates
# No `strip` afterwards: the workspace's release profile already sets strip=true.
# Cargo writes the `dev` profile to target/debug and every other profile to a
# directory of its own name, so the binary is staged at a fixed path for the
# runtime stage.
RUN cargo build --profile "${CARGO_PROFILE}" --locked -p snuffler-web-proxy --bin snuffler-web-proxy \
    && profile_dir="${CARGO_PROFILE}" \
    && if [ "${profile_dir}" = dev ]; then profile_dir=debug; fi \
    && cp "target/${profile_dir}/snuffler-web-proxy" /usr/local/bin/snuffler-web-proxy

FROM ${RUNTIME_IMAGE} AS runtime

ENV DEBIAN_FRONTEND=noninteractive

LABEL org.opencontainers.image.title="snuffler-web-proxy" \
      org.opencontainers.image.description="Allowlisted CORS and WebSocket-to-TCP proxy for snuffler-web"

# ca-certificates because rustls verifies outgoing FDSN requests against the
# system trust store (rustls-platform-verifier); curl only so the HEALTHCHECK
# below has something to ask /health with.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin snuffler

COPY --from=builder /usr/local/bin/snuffler-web-proxy /usr/local/bin/snuffler-web-proxy

# The binary defaults to 127.0.0.1, which inside a container is reachable by
# nothing.
# Everything else keeps its default; the allowlists are extended with
# SNUFFLER_PROXY_ALLOW_HTTP_TARGETS / SNUFFLER_PROXY_ALLOW_TCP_TARGETS.
ENV SNUFFLER_PROXY_BIND=0.0.0.0:17684

USER 10001
EXPOSE 17684

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=5 \
    CMD curl -fsS http://127.0.0.1:17684/health || exit 1

ENTRYPOINT ["snuffler-web-proxy"]
