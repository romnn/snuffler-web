# syntax=docker/dockerfile:1

# The snuffler-web site: the Rust/egui wasm frontend, built by Trunk and served
# by an unprivileged nginx alongside the threaded Pyodide 314 runtime the app
# boots into.
#
# TWO INPUTS ARE NOT BUILT HERE. They are prepared on the host (CI does it in the
# step before `docker build`) and read out of the build context, because building
# them from scratch means an emscripten toolchain and about an hour:
#
#   mise run fetch-pyodide  -> docker/pyodide-mt/served/   (~70 MB: the relinked
#       pthread + OpenMP + SIMD runtime and every wheel, already precompiled to
#       bytecode. docker/pyodide-mt/build-all.sh is what produces one from source.)
#   mise run fetch-pyrocko  -> sources/pyrocko/            (only two paths are
#       read: the cake earthmodels and the map's plate boundaries, both staged
#       into the site by crates/snuffler-web-egui/index.html. pyrocko itself
#       reaches the browser as a wheel inside the runtime above, not from here.)
#
# Everything else — the wasm app, the bundled TS glue, the protobuf code, the
# example dataset bundles — is built in this file.
#
# The layers are ordered cheapest-and-most-stable first, and the two heaviest
# inputs are deliberately kept OUT of the compile stages: the Pyodide runtime
# never enters the builder at all, so swapping runtimes re-runs a compression
# pass over 70 MB rather than a wasm build.
#
#   docker build -f docker/web/Dockerfile -t snuffler-web .

ARG RUST_IMAGE=rust:1-trixie
ARG NGINX_IMAGE=nginx:1.29-alpine
ARG ALPINE_IMAGE=alpine:3.23
ARG CARGO_CHEF_VERSION=0.1.78
# github.com/google/ngx_brotli @ master, 2023-10-09. Pinned by commit: the
# repository publishes no release the module's own docs point at.
ARG NGX_BROTLI_REF=a71f9312c2deb28875acc7bacfdd5695a111aa53

# Every compile stage runs on linux/amd64 whatever the image targets, for two
# reasons that point the same way:
#   - what they emit is architecture-independent — a wasm module, a JS bundle, a
#     zip — so a linux/arm64 image is the same site bytes under a different
#     nginx, and building it twice would buy nothing;
#   - the toolchain is only pinned for amd64. mise.lock declares linux-x64 and
#     macos-arm64, and the binaryen that the wasm-opt hook runs through mise
#     (aqua:WebAssembly/binaryen) has no linux/arm64 entry in aqua's registry at
#     all, so a native arm64 build cannot get the pinned wasm-opt.
# On an amd64 host — GitHub's runners included — this is native for BOTH output
# architectures, so no part of the wasm build is ever emulated. On an arm64 host
# it is emulated and slow; override BUILDER_PLATFORM if you have a native amd64
# builder node to point it at.
ARG BUILDER_PLATFORM=linux/amd64
FROM --platform=${BUILDER_PLATFORM} ${RUST_IMAGE} AS toolchain

# NO_COLOR is "true", not the usual "1": trunk maps the variable onto a clap
# flag that accepts only a boolean literal and rejects anything else outright,
# which is why the mise tasks spell it the same way.
ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_TERM_COLOR=never \
    NO_COLOR=true

# curl for the mise installer, xz/unzip/zstd for the archives mise unpacks.
# git, ca-certificates and a C toolchain come with the rust image.
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl unzip xz-utils zstd \
    && rm -rf /var/lib/apt/lists/*

# mise owns the *web* toolchain (bun, uv, buf, protoc, binaryen, trunk) exactly
# as it does for a local checkout, so the image builds with the versions
# mise.lock pins rather than a second set maintained here.
# MISE_TRUSTED_CONFIG_PATHS rather than `mise trust`: trust is then a property of
# the build, not of a state file a later layer could be missing.
ENV MISE_DATA_DIR=/opt/mise \
    MISE_CACHE_DIR=/opt/mise/cache \
    MISE_STATE_DIR=/opt/mise/state \
    MISE_INSTALL_PATH=/usr/local/bin/mise \
    MISE_TRUSTED_CONFIG_PATHS=/src \
    MISE_YES=1 \
    MISE_PYTHON_COMPILE=0 \
    PATH=/opt/mise/shims:$PATH
RUN curl -fsSL https://mise.run | sh

WORKDIR /src

# Only the tools this build actually runs. Notably absent: node (bun bundles the
# glue), zig and the cargo plugins (lint/test tooling), typos, actionlint.
#
# The GitHub token is optional and passed as a secret so it never lands in a
# layer: mise resolves the trunk/binaryen/buf/protoc releases through the GitHub
# API, whose unauthenticated rate limit is per-IP and therefore shared with every
# other build on a CI runner.
COPY mise.toml mise.lock ./
RUN --mount=type=secret,id=github_token <<'EOF'
set -eu
export MISE_GITHUB_TOKEN="$(cat /run/secrets/github_token 2>/dev/null || true)"
mise install python bun uv buf protoc aqua:WebAssembly/binaryen github:trunk-rs/trunk
mise reshim
EOF

# rustup materializes rust-toolchain.toml on any proxied command, so `rustup
# show` installs the pinned nightly with rust-src and the wasm target — the
# threaded build rebuilds std with atomics (`-Z build-std`), which the
# precompiled wasm32 std cannot provide.
COPY rust-toolchain.toml ./
RUN rustup show

# cargo-chef is installed from a directory WITHOUT rust-toolchain.toml so it
# compiles under the image's own stable toolchain and does not drag the pinned
# nightly into a tool that has nothing to do with this workspace.
ARG CARGO_CHEF_VERSION
WORKDIR /opt/chef
RUN cargo install cargo-chef --locked --version ${CARGO_CHEF_VERSION}
WORKDIR /src

# The recipe is a projection of the manifests and the lock: editing Rust source
# re-runs this stage, but its output only *changes* when a dependency does — and
# BuildKit keys the `COPY --from=planner` below on content, so the expensive cook
# stays cached across ordinary source edits.
FROM toolchain AS planner
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
RUN cargo chef prepare --recipe-path recipe.json

FROM toolchain AS builder

# Compile the dependency graph — every crate the app pulls in, plus the std
# rebuild `-Z build-std` forces — against a skeleton workspace. This is by far
# the most expensive layer and it is invalidated only by Cargo.toml/Cargo.lock.
# The wasm rustflags (atomics, shared memory, the TLS symbol exports) come from
# .cargo/config.toml, so it has to be in place before the cook, not after.
# The cargo registry is left in the layer rather than on a cache mount on
# purpose: a cache mount is invisible to registry/GHA layer caching, so in CI it
# would start empty every run and the restored layers would re-download the index.
ENV CARGO_UNSTABLE_BUILD_STD=std,panic_abort
COPY .cargo .cargo
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json \
    --target wasm32-unknown-unknown -p snuffler-web-egui --features threads

# The bridge glue is bundled by bun from node_modules, and the trunk post-build
# hook stages onnxruntime-web's wasm runtimes out of the same tree.
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile

# `buf generate` shells out to two uv-run plugins (pybuild/protoc-gen-mypy and
# protoc-gen-snuffler-py), which resolve their pinned environments over the
# network on first use. Warm uv's cache in a layer of its own so the build below
# does not pay for it on every source edit. Both plugins read a
# CodeGeneratorRequest from stdin, so they are fed EOF and expected to fail: the
# point is the resolved environment they leave behind.
RUN uvx --from mypy-protobuf==5.1.0 protoc-gen-mypy </dev/null >/dev/null 2>&1 || true
RUN uv run --no-project --with protobuf==7.34.1 python3 -c "import google.protobuf"

# The app's own sources, plus the data it bakes in: the SeisBench weights and the
# example datasets under web/ and examples/, and the two pyrocko files
# index.html stages.
COPY Cargo.toml Cargo.lock buf.yaml buf.gen.yaml ./
COPY crates crates
COPY proto proto
COPY web web
COPY scripts scripts
COPY pybuild pybuild
COPY examples examples
COPY sources/pyrocko/src/data/earthmodels sources/pyrocko/src/data/earthmodels
COPY sources/pyrocko/src/gui/snuffler/snufflings/map/plates.kml \
     sources/pyrocko/src/gui/snuffler/snufflings/map/plates.kml

# Declared here, immediately before the build, so a new commit stamp invalidates
# nothing above it. The app reads both with `option_env!` (see
# crates/snuffler-web-egui/src/view/about.rs) and shows them in its About window;
# leaving them unset builds a stamp-less app, which is what a local build gets.
ARG SNUFFLER_BUILD_DATE=""
ARG SNUFFLER_GIT_COMMIT=""
# Where the site will be mounted. Anything but "/" also needs the matching
# subdirectory under nginx's root, which the final stage handles.
ARG PUBLIC_URL="/"

# Driven directly rather than through `mise run build`, so every knob that task
# sets has to be repeated here: build-std for the threaded wasm, and --no-sri
# because the post-build hook rewrites wasm-bindgen-rayon's worker import after
# Trunk has hashed it.
# The hook prints "[py-mt] MISSING" here and skips staging the runtime — that is
# intended: the runtime is assembled into the image by the `pyodide` stage below.
ENV TRUNK_BUILD_NO_SRI=true
RUN trunk build --config crates/snuffler-web-egui/Trunk.toml \
    --release --features threads --public-url "${PUBLIC_URL}"

# Everything nginx will serve is compressed once, here, rather than per request:
# see docker/web/precompress.sh for what is compressed and why brotli + gzip is
# the whole set.
#
# The two trees are compressed in SEPARATE stages so they cache separately. The
# Pyodide runtime is 70 MB that changes when the runtime is rebuilt — a few times
# a year — while the app changes every commit; sharing a layer would mean
# recompressing the runtime for every source edit.
FROM --platform=$BUILDPLATFORM ${ALPINE_IMAGE} AS compressor
RUN apk add --no-cache brotli gzip findutils
COPY docker/web/precompress.sh /usr/local/bin/precompress

FROM compressor AS site
COPY --from=builder /src/dist/ /site/
RUN precompress /site

FROM compressor AS pyodide
COPY docker/pyodide-mt/served/ /site/py-mt/
RUN precompress /site/py-mt

# nginx serves the precompressed files through brotli_static, which the official
# image has no module for. Modules are built against the exact nginx source and
# `--with-compat`, which is how the upstream image is configured, so the .so
# loads into the stock binary.
# This is the one stage that must run on the TARGET platform — a shared object is
# architecture-specific — but it is keyed only on the nginx version and the
# module pin, so it is built about as often as those change.
FROM ${NGINX_IMAGE} AS nginx-modules
ARG NGX_BROTLI_REF
RUN <<'EOF'
set -eu
apk add --no-cache build-base pcre2-dev zlib-dev openssl-dev linux-headers git curl
curl -fsSL "https://nginx.org/download/nginx-${NGINX_VERSION}.tar.gz" | tar -xz -C /tmp
git clone --quiet https://github.com/google/ngx_brotli.git /tmp/ngx_brotli
git -C /tmp/ngx_brotli checkout --quiet --detach "${NGX_BROTLI_REF}"
cd "/tmp/nginx-${NGINX_VERSION}"
# The static module ONLY — note the path is the `static/` subdirectory, not the
# repository root. Everything this server compresses was compressed at build
# time, so the filter module would be a runtime CPU cost with nothing to do; and
# because serving a .br file needs no encoder, this half of ngx_brotli links no
# Brotli library at all. That is what keeps both the google/brotli submodule out
# of this stage and libbrotli out of the runtime image.
./configure --with-compat --add-dynamic-module=/tmp/ngx_brotli/static
make -j"$(nproc)" modules
mkdir -p /modules
cp objs/ngx_http_brotli_static_module.so /modules/
EOF

FROM ${NGINX_IMAGE} AS runtime

ARG PUBLIC_URL="/"

LABEL org.opencontainers.image.title="snuffler-web" \
      org.opencontainers.image.description="Pyrocko's snuffler seismic waveform inspector, in the browser"

COPY --from=nginx-modules /modules/ngx_http_brotli_static_module.so /usr/lib/nginx/modules/
COPY docker/web/nginx/nginx.conf /etc/nginx/nginx.conf
COPY docker/web/nginx/default.conf /etc/nginx/conf.d/default.conf
COPY docker/web/nginx/snippets/ /etc/nginx/snippets/
COPY --from=site /site/ /usr/share/nginx/html${PUBLIC_URL}
COPY --from=pyodide /site/py-mt/ /usr/share/nginx/html${PUBLIC_URL}py-mt/

# Non-root, and nothing in the image needs to be writable for it: nginx.conf puts
# the pid and every temp path under /tmp, and the logs are the base image's
# symlinks to /dev/stdout and /dev/stderr.
# 101 is the `nginx` user the upstream image creates; naming the uid numerically
# keeps the container non-root under a Kubernetes `runAsNonRoot` check, which
# cannot resolve a username.
USER 101

# 8080 rather than 80 so no capability is needed to bind it.
EXPOSE 8080

# The image's own entrypoint scripts assume they run as root (they rewrite
# configs and chown paths); this server needs none of what they do.
STOPSIGNAL SIGQUIT
ENTRYPOINT ["nginx"]
CMD ["-g", "daemon off;"]

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -q --spider http://127.0.0.1:8080/health || exit 1
