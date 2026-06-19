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

## Architecture

```
 Browser
 ├─ Pyodide (CPython + numpy + scipy + pyrocko, all WASM)        ← the compute
 │    └─ web/bridge.py   — type-annotated pyrocko shim (the stable API)
 ├─ web/bridge.js        — boots Pyodide, installs the wheel, marshals arrays
 └─ egui app (Rust → wasm32, via Trunk/wasm-bindgen)             ← the UI
      └─ src/bridge.rs   — async wasm-bindgen calls into bridge.js; serde structs
      └─ src/view/*      — stacked waveform canvas, time axis, controls, markers
```

The egui frontend asks the bridge for a *view* (a time window + filters + a
target pixel width); pyrocko filters and decimates the traces and returns compact
`float32` arrays + JSON metadata. The data contract is defined once in
`web/bridge.py` (`TypedDict`s) and mirrored by the serde structs in
`src/bridge.rs`, so the glue is type-checked on both sides.

**pyrocko is used unmodified.** We replace only its *build system* for the web:
`pybuild/` cross-compiles just the two C extensions the inspector needs
(`signal_ext`, and `mseed_ext` + bundled `libmseed`) into a Pyodide wheel; the
rest of pyrocko ships as its normal pure-Python tree.

## Toolchain (clone → mise → run)

Everything is pinned in `mise.toml` + `rust-toolchain.toml`. With
[mise](https://mise.jdx.dev) installed:

```sh
mise install          # Python, emscripten, node, bun, uv, trunk (all pinned)
mise run setup        # fetch pyrocko (pinned commit) + create .venv-pyodide
mise run build-wheel  # cross-compile pyrocko → dist-py/pyrocko-*.whl
mise run serve        # trunk serve  → http://127.0.0.1:8080
```

`rustup` reads `rust-toolchain.toml` and installs the pinned Rust toolchain plus
the `wasm32-unknown-unknown` target automatically on first build.

**pyrocko is not vendored.** `mise run fetch-pyrocko` clones it at a pinned,
known-good commit (`PYROCKO_REF` in `mise.toml`) into the gitignored `sources/`;
`setup`, `build-wheel`, and `serve` all depend on it, so it is pulled on demand —
no manual step needed. To validate the cross-compiled wheel against the real
Pyodide runtime, run `mise run smoke-test` (headless: imports pyrocko, filters a
trace, loads MiniSEED).

The whole version chain is dictated by the Pyodide **runtime** loaded in
`web/bridge.js` (0.27.2): runtime → `pyodide-build` 0.35.0 → cross-build env
0.27.2 → emscripten 3.1.58 → host CPython 3.12.

## Layout

| Path | What |
|---|---|
| `src/` | egui frontend (`app.rs`, `bridge.rs`, `view/*`, `state.rs`) |
| `web/bridge.js` | JS glue: Pyodide boot, wheel install, array marshaling |
| `web/bridge.py` | type-checked pyrocko shim (`make_example`, `load_bytes`, `get_view`) |
| `pybuild/` | wheel cross-compile + `fetch-pyrocko.sh` + toolchain bootstrap |
| `dist-py/` | output wheel + `wheels.json` (served at `/py/`); gitignored, rebuilt by `build-wheel` |
| `sources/pyrocko/` | upstream pyrocko — **fetched on demand at a pinned commit, gitignored (not vendored)** |

## Notes & limitations

- First load pulls Pyodide + numpy/scipy/matplotlib + the pyrocko wheel
  (~100 MB, dominated by scipy/matplotlib) from the jsdelivr CDN; subsequent
  loads are cached. See `plan/load-time-reduction.md` for how to cut that.
- matplotlib is loaded only because `pyrocko.orthodrome` imports
  `matplotlib.path` at module top; it isn't otherwise used.
- The native build (`cargo run`) uses a small synthetic stub instead of Pyodide,
  for fast UI iteration without a browser (waterfall/spectrum are web-only there).
- **Response deconvolution** (Options → Remove response) is wired through
  `trace.transfer` but is a no-op until response metadata (StationXML/RESP) is
  loaded — the demo data carries none.
- Snufflings: a built-in amplitude-spectrum tool and the spectrogram are
  provided; the full dynamic plugin framework is not ported.
