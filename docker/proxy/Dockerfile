# syntax=docker/dockerfile:1

# snuffler-web-proxy: the allowlisted CORS + WebSocket-to-TCP proxy the app talks
# to for non-CORS FDSN endpoints and for SeedLink/ringserver streams.
# Same binary micromux runs locally; the container listens on 8080 like the web image,
# and docker-compose.yaml publishes it on the host's 9000.
#
# Caching is cargo-chef's: the dependency graph is compiled against a skeleton
# workspace in a layer keyed only on Cargo.toml/Cargo.lock, so editing the proxy
# recompiles the proxy and nothing else.
# The cargo registry is left in the layer rather than on a cache mount on purpose
# — a cache mount is invisible to registry/GHA layer caching, so in CI it would
# start empty on every run and the restored layers would re-download the whole
# index.
# All Rust and C compilation runs on BUILDPLATFORM through cargo-zigbuild.
# The runtime has no build commands, so neither architecture needs emulation.
#
#   docker build -f docker/proxy/Dockerfile -t snuffler-proxy .
#
# CARGO_PROFILE picks the Cargo profile.
# Release images use the default; CI builds every untagged commit with `dev`,
# which compiles much faster and is still enough for the smoke test.
#
#   docker build -f docker/proxy/Dockerfile --build-arg CARGO_PROFILE=dev .

ARG RUST_IMAGE=rust:1-trixie
ARG RUNTIME_IMAGE=gcr.io/distroless/static-debian13:nonroot
ARG CARGO_CHEF_VERSION=0.1.78

FROM --platform=$BUILDPLATFORM ${RUST_IMAGE} AS toolchain

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_TERM_COLOR=never

# cmake builds aws-lc-sys, the C library behind reqwest's rustls backend.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake curl xz-utils \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Use the same pinned cross toolchain as local builds and linting.
# Keep it in ordinary layers so registry cache exports include the tools.
ENV MISE_DATA_DIR=/opt/mise \
    MISE_CACHE_DIR=/opt/mise/cache \
    MISE_STATE_DIR=/opt/mise/state \
    MISE_INSTALL_PATH=/usr/local/bin/mise \
    MISE_TRUSTED_CONFIG_PATHS=/src \
    MISE_YES=1 \
    PATH=/opt/mise/shims:$PATH
RUN curl -fsSL https://mise.run | sh
COPY mise.toml mise.lock ./
RUN --mount=type=secret,id=github_token <<'EOF'
set -eu
export MISE_GITHUB_TOKEN="$(cat /run/secrets/github_token 2>/dev/null || true)"
mise install zig github:rust-cross/cargo-zigbuild
mise reshim
EOF

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
ARG TARGETARCH

# BuildKit architecture names differ from Rust's target triples.
# Install std only after selecting the workspace's pinned toolchain.
RUN case "${TARGETARCH}" in \
        amd64) target=x86_64-unknown-linux-musl ;; \
        arm64) target=aarch64-unknown-linux-musl ;; \
        *) echo "Unsupported proxy architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && echo "${target}" > /opt/rust-target \
    && rustup target add "${target}"

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
    cargo chef cook --zigbuild --target "$(cat /opt/rust-target)" \
    --profile "${CARGO_PROFILE}" --locked --recipe-path recipe.json -p snuffler-web-proxy

COPY Cargo.toml Cargo.lock ./
# Keep real manifests for lint inheritance, but omit unrelated UI sources so
# editing them cannot invalidate this compile layer.
COPY crates/accesskit-dom/Cargo.toml crates/accesskit-dom/Cargo.toml
COPY crates/egui-a11y/Cargo.toml crates/egui-a11y/Cargo.toml
COPY crates/snuffler-web-core/Cargo.toml crates/snuffler-web-core/Cargo.toml
COPY crates/snuffler-web-egui/Cargo.toml crates/snuffler-web-egui/Cargo.toml
COPY crates/snuffler-web-proto/Cargo.toml crates/snuffler-web-proto/Cargo.toml
COPY crates/snuffler-web-render/Cargo.toml crates/snuffler-web-render/Cargo.toml
COPY crates/cors-proxy crates/cors-proxy
COPY crates/tcp-over-ws-proxy crates/tcp-over-ws-proxy
COPY crates/snuffler-web-proxy crates/snuffler-web-proxy
# No `strip` afterwards: the workspace's release profile already sets strip=true.
# Cargo writes the `dev` profile to target/debug and every other profile to a
# directory of its own name, so the binary is staged at a fixed path for the
# runtime stage.
RUN cargo zigbuild --target "$(cat /opt/rust-target)" \
    --profile "${CARGO_PROFILE}" --locked -p snuffler-web-proxy --bin snuffler-web-proxy \
    && profile_dir="${CARGO_PROFILE}" \
    && if [ "${profile_dir}" = dev ]; then profile_dir=debug; fi \
    && cp "target/$(cat /opt/rust-target)/${profile_dir}/snuffler-web-proxy" /usr/local/bin/snuffler-web-proxy

# The musl binary is static; only CA certificates and basic system data are needed.
# No libc shared libraries, OpenSSL, shell or package manager are shipped.
FROM ${RUNTIME_IMAGE} AS runtime

LABEL org.opencontainers.image.title="snuffler-web-proxy" \
      org.opencontainers.image.description="Allowlisted CORS and TCP-over-WebSocket proxy for snuffler-web"

COPY --from=builder --chmod=0555 /usr/local/bin/snuffler-web-proxy /usr/local/bin/snuffler-web-proxy

# The binary defaults to 127.0.0.1:9000, which inside a container is reachable by nothing.
# Its log defaults to readable lines for a terminal; a container's goes to a log shipper.
ENV SNUFFLER_PROXY_BIND=0.0.0.0:8080 \
    SNUFFLER_PROXY_LOG_FORMAT=json

USER 65532:65532
EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=5 \
    CMD ["/usr/local/bin/snuffler-web-proxy", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/snuffler-web-proxy"]
