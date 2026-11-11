# snuffler-web

[Snuffler](https://pyrocko.org) — pyrocko's seismic waveform inspector — running
**in the browser**. The heavy lifting is still done by real **pyrocko + numpy +
scipy** (compiled to WebAssembly via [Pyodide](https://pyodide.org)); only the UI
is new: a [Rust/egui](https://github.com/emilk/egui) frontend compiled to WASM.
We give snuffler a new outfit without touching its brain.

Loads MiniSEED, SAC and the other formats pyrocko reads, from files, FDSN
services or a live SeedLink stream; shows them as stacked wiggles or a waterfall
with snuffler's filters, rotation, scaling, sorting and markers; and runs ports of
snuffler's built-in snufflings (spectra, spectrogram, polarization, STA/LTA, cake,
seismosizer, SeisBench pickers). **`File ▸ Examples`** loads real recorded
datasets on demand — each one a link you can hand out (`?example=<slug>`), see
[`examples/`](examples/).

## The original Qt snuffler

The web app is a Rust/egui UI compiled to wasm that drives real pyrocko via Pyodide,
running **entirely in the browser, no server** (`task serve`).
The real, unmodified PyQt5 snuffler is not shipped: Qt-for-Python has no WebAssembly
build (see [ADR 2](adr/0002-upstream-qt-is-a-reference.md)).
It runs natively instead, as the reference the parity tests measure the web app
against: `task snuffler`, see [`qt/`](qt/).

## Architecture

```
 Browser
 ├─ Web Worker
 │  └─ Pyodide (CPython + numpy + scipy + pyrocko, all WASM)     ← the compute
 │     └─ web/snuffler_bridge/  — strict-typed pyrocko shim
 ├─ web/bridge.ts        — main-thread client, DOM I/O, worker RPC
 ├─ proto/               — protobuf bridge schema, generated for Rust/Python
 └─ Rust frontend (wasm32, via Trunk/wasm-bindgen)               ← the UI
      ├─ crates/snuffler-web-core/    — session state + async protobuf bridge (no egui)
      ├─ crates/snuffler-web-render/  — waveform painting + SVG export (no egui)
      └─ crates/snuffler-web-egui/    — egui widgets, input handling, Trunk entry point
```

The egui frontend asks the bridge for a *view* (a time window + filters + a
target pixel width); pyrocko filters and decimates the traces and returns compact
`float32` arrays + protobuf metadata. The data contract is defined once in
`proto/snuffler/bridge/v1/*.proto`; `prost` type-checks the Rust
side and `buf generate` emits Python code plus mypy stubs for the Pyodide side.

**pyrocko is used unmodified.** We replace only its *build system* for the web:
`pybuild/setup.py` cross-compiles its C extensions (all but `evalresp_ext`, whose
C library is not bundled) into a Pyodide wheel; the rest of pyrocko ships as its
normal pure-Python tree.

## Toolchain (clone → mise → Task)

Everything is pinned in `mise.toml` + `rust-toolchain.toml`. With
[mise](https://mise.jdx.dev) installed:

```sh
mise install                    # node, bun, uv, trunk, binaryen, protoc (all pinned)
task pyodide:fetch              # download the deployed custom Pyodide 314 into py-mt
task serve                      # trunk serve → http://127.0.0.1:17683
micromux                        # optional full local stack: proxy + trunk serve
```

Mise installs and selects pinned dependencies.
Task is the single command surface for building, serving, checking, and testing
the repository; `task --list` shows every available workflow.

Python tooling uses the locked Python 3.14 project at the repository root.
`uv run` creates and updates `.venv` automatically, so no activation or separate
sync step is required:

```sh
task lint:py
task check:py
```

The native Qt application and its parity tests use a separate locked Python 3.12
project under `qt/`:

```sh
task snuffler                         # fetch pinned pyrocko and launch its native GUI
task snuffler:example EXAMPLE=kilauea-2018-07-15-caldera-collapse
uv sync --project qt                 # optional: prepare qt/.venv without launching
task test:parity:unit
task test:parity:bridge
```

`pybuild/pyproject.toml` remains independent because it describes the Pyodide
wheel build rather than a host development environment.

`rustup` reads `rust-toolchain.toml` and installs the pinned nightly toolchain,
`rust-src`, and the `wasm32-unknown-unknown` target automatically on first build.

**`serve` builds optimized.** An unoptimized wasm frontend renders at 10-20 fps,
so `task serve` uses the `serve` cargo profile (`Cargo.toml`): release-grade
codegen, but without the fat LTO and `strip` that would make every rebuild in the
watch loop slow and every browser panic unreadable. The first build after
switching profiles recompiles the whole dependency graph (including `std`, which
`-Z build-std` rebuilds for wasm atomics); after that, rebuilds are incremental.

**pyrocko is not vendored.** `task fetch:pyrocko` clones it at a pinned,
known-good commit (`PYROCKO_REF` in `mise.toml`) into the gitignored `sources/`
(`build-all.sh` and `serve` depend on it, so it is pulled on demand).

**The runtime can be downloaded instead of built.** Pages publishes exactly the
directory `stage-web.sh` assembles, so `task pyodide:fetch` pulls the deployed
runtime into `docker/pyodide-mt/served/` in seconds — no Docker, no emsdk, no
hour. Build it with `task pyodide:build` when the runtime itself is what you are
changing: a patch, a package pin, or `pybuild/setup.py`.

**The app runs our custom Pyodide 314** (py3.14 / emscripten 5.0.3, pthread +
OpenMP + SIMD), built in `docker/pyodide-mt` (`build-all.sh`, the multi-hour but
cacheable step) and served at `py-mt/`. No host emsdk/xbuildenv is involved; the
wheels are built inside the pyodide-env image.

## Docker

Both services in this repo ship as container images, published to the GitHub
Container Registry on each release tag (`.github/workflows/docker.yaml`); every
other push and pull request only builds and smoke-tests them:

| Image | What |
|---|---|
| `ghcr.io/romnn/snuffler-web/web` | the site — egui wasm frontend, Pyodide 314 runtime, example bundles — behind an unprivileged nginx on `:8080` |
| `ghcr.io/romnn/snuffler-web/proxy` | `snuffler-web-proxy` on `:17684`: the allowlisted `/cors` and `/tcp` endpoints |

`latest`, `X.Y.Z` and `X.Y` follow the newest release tag, and every release is
additionally tagged `sha-<commit>`.
Both images are published for `linux/amd64` and `linux/arm64`.

```sh
docker compose up            # site on http://127.0.0.1:17683, proxy on :17684
docker compose up --build    # build both from this checkout instead
```

The proxy is published on the **host's** 17684 for a reason: the app resolves it
in the browser — `web/snuffler_bridge/fdsn.py` and `snuffler-web-core` both name
`127.0.0.1:17684`, and only route through it when the page is on a localhost
origin — so the two containers never talk to each other.

### Building them

The wasm frontend is built inside the image. The two heavy inputs are not, and
must be on disk first — the same two commands a local checkout runs:

```sh
task fetch:pyrocko    # sources/pyrocko: two data files end up in the site
task pyodide:fetch    # docker/pyodide-mt/served: the ~70 MB Pyodide runtime
task docker:build
```

`task docker:build` fetches pyrocko for you and checks for
the Pyodide runtime rather than re-downloading it, so a locally *built* one is
left alone. `task docker:serve` is `docker compose up`.

Both Dockerfiles are laid out for cache reuse: `cargo-chef` compiles the
dependency graph (including the `-Z build-std` std rebuild) in a layer keyed only
on `Cargo.toml`/`Cargo.lock`, the mise toolchain and the JS dependencies get
layers of their own, and the Pyodide runtime never enters the compile stages at
all — so changing the runtime re-runs a copy, not a wasm build.

The web image's compile stages are pinned to `linux/amd64` for both output
architectures — what they emit is architecture-independent, and `mise.lock` pins
the toolchain for `linux-x64` only (aqua has no `linux/arm64` binaryen at all).
On an amd64 host, and on GitHub's runners, nothing is emulated; on an arm64 host
those stages run under emulation and are slow.

### What the web image does that a plain static host does not

- Sets `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy`, so the
  page is cross-origin isolated from the first byte and `coi-serviceworker.js`
  finds nothing left to do — unlike GitHub Pages, which cannot set headers.
- Serves brotli and gzip copies compressed once at build time (`brotli -q 11`)
  through `brotli_static`/`gzip_static`, and keeps a copy only where it actually
  saves something — see [`docker/web/precompress.sh`](docker/web/precompress.sh).
- Gives `.mjs` a `text/javascript` type, without which the browser refuses
  Pyodide's loader outright.
- Caches the content-hashed assets immutably and everything else with
  `no-cache`, which is "revalidate", not "do not store": most of the site keeps a
  stable url while its bytes change per build.
- Runs as uid 101 on port 8080 with a read-only root filesystem — nothing outside
  `/tmp` is written.

## Layout

| Path | What |
|---|---|
| `web/bridge.ts` | main-thread client for the Pyodide worker; the DOM glue beside it reads files (`browser_files.ts`), saves exports (`save_file.ts`) and fetches map tiles (`map_tiles.ts`) |
| `web/bridge_worker.ts` | the Pyodide worker: boot, package install, protobuf RPC relay, cancellation; downloads in `pyodide_prefetch.ts`, progress events in `worker_events.ts`, SeisBench inference in `ort_client.ts` |
| `web/snuffler_bridge/` | the type-checked pyrocko side of every bridge RPC, one module per concern |
| `proto/` | protobuf schema for the Rust/Python bridge |
| `adr/` | architecture decision records |
| `examples/` | real recorded datasets: `File ▸ Examples` fetches each as one bundle, and the parity suite measures against the same bytes |
| `crates/snuffler-web-core/` | shared session state and Pyodide bridge |
| `crates/snuffler-web-render/` | toolkit-neutral drawing and SVG backend |
| `crates/snuffler-web-egui/` | egui frontend, Trunk entry point, and frontend assets |
| `crates/snuffler-web-proto/` | generated Rust protobuf crate |
| `crates/accesskit-dom/`, `crates/egui-a11y/` | the AccessKit tree projected into a DOM overlay, for screen readers and browser agents |
| `crates/snuffler-web-proxy/` | snuffler-web's local `/cors` + `/tcp` proxy, with the hosts the app uses built in |
| `crates/cors-proxy/`, `crates/tcp-over-ws-proxy/` | the generic, allowlisted proxies it is built from |
| `qt/` | the upstream Qt snuffler as a native reference, with a headless driver |
| `tests/` | browser suites and the Qt parity harness (`tests/parity/`) |
| `pybuild/` | `setup.py` (the pyrocko wheel build def, used by `docker/pyodide-mt`) + `fetch-pyrocko.sh` + `protoc-gen-mypy` |
| `docker/pyodide-mt/` | the custom Pyodide 314 build (runtime + wheels): `build-all.sh` |
| `docker/web/` | the site image: Trunk build + unprivileged nginx, with its config and the build-time compressor |
| `docker/proxy/` | the `snuffler-web-proxy` image |
| `docker-compose.yaml` | both of the above on the ports micromux uses locally |
| `sources/pyrocko/` | upstream pyrocko — **fetched on demand at a pinned commit, gitignored (not vendored)** |

## Notes & limitations

- First load pulls the Pyodide runtime with numpy, scipy, matplotlib and the
  pyrocko wheel, dominated by scipy and matplotlib; later loads are cached.
  scipy carries pyrocko's filters and matplotlib renders pyrocko's own figures.
- The egui frontend is browser-only: run it with `task serve` or pass
  `--config crates/snuffler-web-egui/Trunk.toml` to Trunk,
  not `cargo run`.
- For non-CORS FDSN endpoints or browser-accessible SeedLink/ringserver streams,
  run `micromux` instead of plain `task serve`. It starts Trunk on
  `127.0.0.1:17683` and `snuffler-web-proxy` on `127.0.0.1:17684`; local FDSN fetches
  use `/cors` through that proxy when available, and the same process exposes
  `/tcp` as a TCP-over-WebSocket tunnel for the SeedLink stream.
- `snuffler-web-proxy` is not an open proxy: `/cors` only forwards to the FDSN
  query endpoints the app uses, only permits `GET`/`HEAD`, and strips browser
  credential headers; `/tcp` only connects to the app's SeedLink servers. Both lists
  are in `crates/snuffler-web-proxy/src/defaults.rs`. A deployment allows more with
  `--allow-http-target` / `--allow-tcp-target`, their variables
  `SNUFFLER_PROXY_ALLOW_HTTP_TARGETS` / `SNUFFLER_PROXY_ALLOW_TCP_TARGETS`, or a
  `.env` next to the proxy (see `.env.example`). The proxy itself is built from two
  generic crates, `cors-proxy` and `tcp-over-ws-proxy`, which know no hosts.
- **Response deconvolution** (Options → Remove response) is wired through
  `trace.transfer` but is a no-op until response metadata (StationXML/RESP) is
  loaded — the demo data carries none.
- Snufflings are ported one by one; user snuffling directories are not loaded,
  because the browser page has none.
