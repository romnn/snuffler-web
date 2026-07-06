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
> themes**, and a progress-bar loading screen.

## Two frontends

There are two independent ways to run snuffler in the browser; they coexist with
separate commands and artifacts:

- **egui — client-side (default).** A Rust/egui UI compiled to wasm that drives real
  pyrocko via Pyodide, running **entirely in the browser, no server**. This is what
  the rest of this README covers (`mise run serve`).
- **Original Qt — server-streamed (optional).** The *real, unmodified* PyQt5
  snuffler, rendered headless and streamed to the browser over noVNC — 100%
  feature-complete, but runs **server-side** (Docker): `mise run qt-build` /
  `qt-serve`, see [`qt/`](qt/). Qt-for-Python has no WebAssembly build, so this one
  can't be client-side — see [`plan/qt-frontend-feasibility.md`](plan/qt-frontend-feasibility.md).

## Architecture

```
 Browser
 ├─ Web Worker
 │  └─ Pyodide (CPython + numpy + scipy + pyrocko, all WASM)     ← the compute
 │     └─ web/bridge.py  — strict-typed pyrocko shim
 ├─ web/bridge.js        — main-thread client, DOM I/O, worker RPC
 ├─ proto/               — protobuf bridge schema, generated for Rust/Python
 └─ egui app (Rust → wasm32, via Trunk/wasm-bindgen)             ← the UI
      └─ src/bridge.rs   — async wasm-bindgen calls into bridge.js; protobuf bytes
      └─ src/view/*      — stacked waveform canvas, time axis, controls, markers
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

## Toolchain (clone → mise → run)

Everything is pinned in `mise.toml` + `rust-toolchain.toml`. With
[mise](https://mise.jdx.dev) installed:

```sh
mise install                    # node, bun, uv, trunk, binaryen, protoc (all pinned)
docker/pyodide-mt/build-all.sh  # build the custom Pyodide 314 (both wheels + py-mt)
mise run serve                  # trunk serve → http://127.0.0.1:17683
micromux                        # optional full local stack: proxy + trunk serve
```

`rustup` reads `rust-toolchain.toml` and installs the pinned nightly toolchain,
`rust-src`, and the `wasm32-unknown-unknown` target automatically on first build.

**pyrocko is not vendored.** `mise run fetch-pyrocko` clones it at a pinned,
known-good commit (`PYROCKO_REF` in `mise.toml`) into the gitignored `sources/`
(`build-all.sh` and `serve` depend on it, so it is pulled on demand).

**Both Pyodide paths run our custom Pyodide 314** (py3.14 / emscripten 5.0.3),
built in `docker/pyodide-mt` (`build-all.sh`, the multi-hour but cacheable step):
the threaded `py-mt/` runtime (pthread + OpenMP + SIMD, reachable via `?threads=1`
+ the coi-serviceworker) and a single-threaded `dist-py/` pyrocko wheel for the
stock 314.0.0 CDN fallback — differing only in multithreading + SIMD. No host
emsdk/xbuildenv is involved; the wheels are built inside the pyodide-env image.

## Layout

| Path | What |
|---|---|
| `src/` | egui frontend (`app.rs`, `bridge.rs`, `view/*`, `state.rs`) |
| `web/bridge.js` | main-thread JS client for the Pyodide worker |
| `web/bridge_worker.js` | Pyodide boot, wheel install, protobuf RPC, buffer transfer |
| `web/bridge.py` | type-checked pyrocko shim (`make_example`, `load_bytes`, `get_view`) |
| `proto/` | protobuf schema for the Rust/Python bridge |
| `crates/snuffler-proto/` | generated Rust protobuf crate |
| `pybuild/` | `setup.py` (the pyrocko wheel build def, used by `docker/pyodide-mt`) + `fetch-pyrocko.sh` + `protoc-gen-mypy` |
| `docker/pyodide-mt/` | the custom Pyodide 314 build (runtime + both wheels): `build-all.sh` |
| `dist-py/` | single-threaded CDN pyrocko wheel + `wheels.json` (served at `/py/`); gitignored, built by `build-all.sh` |
| `sources/pyrocko/` | upstream pyrocko — **fetched on demand at a pinned commit, gitignored (not vendored)** |

## Notes & limitations

- First load pulls Pyodide + numpy/scipy/matplotlib + the pyrocko wheel
  (~100 MB, dominated by scipy/matplotlib) from the jsdelivr CDN; subsequent
  loads are cached. See `plan/load-time-reduction.md` for how to cut that.
- matplotlib is loaded only because `pyrocko.orthodrome` imports
  `matplotlib.path` at module top; it isn't otherwise used.
- The egui frontend is browser-only: run it with `mise run serve` / `trunk serve`,
  not `cargo run`.
- For non-CORS FDSN endpoints or browser-accessible SeedLink/ringserver streams,
  run `micromux` instead of plain `mise run serve`. It starts Trunk on
  `127.0.0.1:17683` and `snuffler-proxy` on `127.0.0.1:17684`; local FDSN fetches
  use `/cors` through that proxy when available, and the same process exposes
  `/seedlink` and `/tcp` as WebSocket-to-raw-TCP tunnels.
- `snuffler-proxy` is not an open proxy: `/cors` only forwards to the known FDSN
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
