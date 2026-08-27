# snuffler-web

[Snuffler](https://pyrocko.org) — pyrocko's seismic waveform inspector — running
**in the browser**. The heavy lifting is still done by real **pyrocko + numpy +
scipy** (compiled to WebAssembly via [Pyodide](https://pyodide.org)); only the UI
is new: a [Rust/egui](https://github.com/emilk/egui) frontend compiled to WASM.
We give snuffler a new outfit without touching its brain.

> Status: a working app. Loads synthetic data, SAC, and real MiniSEED; stacked
> wiggle display **and a waterfall/spectrogram view**, adaptive time axis, NSLC
> labels, highpass/lowpass/gain, **component rotation**, scaling & sorting,
> pan/zoom, markers, an **amplitude-spectrum snuffling**, **light/dark/system
> themes**, and a progress-bar loading screen. **`File ▸ Examples`** loads real
> recorded datasets on demand — each one a link you can hand out
> (`?example=<slug>`), see [`examples/`](examples/).

## Two frontends

There are two independent ways to run snuffler in the browser; they coexist with
separate commands and artifacts:

- **egui — client-side (default).** A Rust/egui UI compiled to wasm that drives real
  pyrocko via Pyodide, running **entirely in the browser, no server**. This is what
  the rest of this README covers (`task serve`).
- **Original Qt — server-streamed (optional).** The *real, unmodified* PyQt5
  snuffler, rendered headless and streamed to the browser over noVNC — 100%
  feature-complete, but runs **server-side** (Docker): `task qt:build` /
  `task qt:serve`, see [`qt/`](qt/). Qt-for-Python has no WebAssembly build, so this one
  can't be client-side — see [`plan/qt-frontend-feasibility.md`](plan/qt-frontend-feasibility.md).

## Architecture

```
 Browser
 ├─ Web Worker
 │  └─ Pyodide (CPython + numpy + scipy + pyrocko, all WASM)     ← the compute
 │     └─ web/bridge.py  — strict-typed pyrocko shim
 ├─ web/bridge.js        — main-thread client, DOM I/O, worker RPC
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
`pybuild/` cross-compiles just the two C extensions the inspector needs
(`signal_ext`, and `mseed_ext` + bundled `libmseed`) into a Pyodide wheel; the
rest of pyrocko ships as its normal pure-Python tree.

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

**Both Pyodide paths run our custom Pyodide 314** (py3.14 / emscripten 5.0.3),
built in `docker/pyodide-mt` (`build-all.sh`, the multi-hour but cacheable step):
the threaded `py-mt/` runtime (pthread + OpenMP + SIMD, reachable via `?threads=1`
+ the coi-serviceworker) and a single-threaded `dist-py/` pyrocko wheel for the
stock 314.0.0 CDN fallback — differing only in multithreading + SIMD. No host
emsdk/xbuildenv is involved; the wheels are built inside the pyodide-env image.

## Docker

Every service in this repo ships as a container image, built and published to the
GitHub Container Registry on each push to `main` and each tag
(`.github/workflows/docker.yaml`):

| Image | What |
|---|---|
| `ghcr.io/romnn/snuffler-web/web` | the site — egui wasm frontend, Pyodide 314 runtime, example bundles — behind an unprivileged nginx on `:8080` |
| `ghcr.io/romnn/snuffler-web/proxy` | `snuffler-web-proxy` on `:17684`: the allowlisted `/cors` and `/seedlink` endpoints |
| `ghcr.io/romnn/snuffler-web/qt` | the original PyQt5 snuffler over noVNC on `:8080` (see [`qt/`](qt/)) |

`latest`, `X.Y.Z` and `X.Y` follow the newest release tag; `edge` and `main`
follow the default branch; every build is additionally tagged `sha-<commit>`.
`web` and `proxy` are published for `linux/amd64` and `linux/arm64`.

```sh
docker compose up            # site on http://127.0.0.1:17683, proxy on :17684
docker compose up --build    # build both from this checkout instead
```

The proxy is published on the **host's** 17684 for a reason: the app resolves it
in the browser — `web/bridge.py` and `snuffler-web-core` both name
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
| `web/bridge.js` | main-thread JS client for the Pyodide worker |
| `web/bridge_worker.js` | Pyodide boot, wheel install, protobuf RPC, buffer transfer |
| `web/bridge.py` | type-checked pyrocko shim (`make_example`, `load_bytes`, `get_view`) |
| `proto/` | protobuf schema for the Rust/Python bridge |
| `examples/` | real recorded datasets: `File ▸ Examples` fetches each as one bundle, and the parity suite measures against the same bytes |
| `crates/snuffler-web-core/` | shared session state and Pyodide bridge |
| `crates/snuffler-web-render/` | toolkit-neutral drawing and SVG backend |
| `crates/snuffler-web-egui/` | egui frontend, Trunk entry point, and frontend assets |
| `crates/snuffler-web-proto/` | generated Rust protobuf crate |
| `pybuild/` | `setup.py` (the pyrocko wheel build def, used by `docker/pyodide-mt`) + `fetch-pyrocko.sh` + `protoc-gen-mypy` |
| `docker/pyodide-mt/` | the custom Pyodide 314 build (runtime + both wheels): `build-all.sh` |
| `docker/web/` | the site image: Trunk build + unprivileged nginx, with its config and the build-time compressor |
| `docker/proxy/` | the `snuffler-web-proxy` image |
| `docker-compose.yaml` | both of the above on the ports micromux uses locally |
| `dist-py/` | single-threaded CDN pyrocko wheel + `wheels.json` (served at `/py/`); gitignored, built by `build-all.sh` |
| `sources/pyrocko/` | upstream pyrocko — **fetched on demand at a pinned commit, gitignored (not vendored)** |

## Notes & limitations

- First load pulls Pyodide + numpy/scipy/matplotlib + the pyrocko wheel
  (~100 MB, dominated by scipy/matplotlib) from the jsdelivr CDN; subsequent
  loads are cached. See `plan/load-time-reduction.md` for how to cut that.
- matplotlib is loaded only because `pyrocko.orthodrome` imports
  `matplotlib.path` at module top; it isn't otherwise used.
- The egui frontend is browser-only: run it with `task serve` or pass
  `--config crates/snuffler-web-egui/Trunk.toml` to Trunk,
  not `cargo run`.
- For non-CORS FDSN endpoints or browser-accessible SeedLink/ringserver streams,
  run `micromux` instead of plain `task serve`. It starts Trunk on
  `127.0.0.1:17683` and `snuffler-web-proxy` on `127.0.0.1:17684`; local FDSN fetches
  use `/cors` through that proxy when available, and the same process exposes
  `/seedlink` and `/tcp` as WebSocket-to-raw-TCP tunnels.
- `snuffler-web-proxy` is not an open proxy: `/cors` only forwards to the known FDSN
  query endpoints used by the app, only permits `GET`/`HEAD`, and strips browser
  credential headers. `/seedlink`/`/tcp` only connect to known SeedLink/ringserver
  hosts on their allowed ports. A hosted deployment can add exact targets with
  `--allow-http-target` / `SNUFFLER_PROXY_ALLOW_HTTP_TARGETS` and
  `--allow-tcp-target` / `SNUFFLER_PROXY_ALLOW_TCP_TARGETS`.
- **Response deconvolution** (Options → Remove response) is wired through
  `trace.transfer` but is a no-op until response metadata (StationXML/RESP) is
  loaded — the demo data carries none.
- Snufflings: a built-in amplitude-spectrum tool and the spectrogram are
  provided; the full dynamic plugin framework is not ported.
