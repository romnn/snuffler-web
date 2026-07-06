# Implementation-agent prompt — WebGPU multi-model ML inference

Paste everything below the line to an implementation agent working in
`/home/roman/dev/snuffler-web`.

---

You are implementing **fast, multi-model, GPU-accelerated ML phase picking** in
snuffler-web (a Rust/egui + Pyodide browser app). Today the app ships one model
(SeisBench PhaseNet `instance`) run by a hand-written pure-numpy ONNX interpreter
(`web/onnx_infer.py`) — CPU-only, single model. Replace that with
**onnxruntime-web using the WebGPU execution provider** so the whole SeisBench
model zoo runs, GPU-accelerated, with models fetched on demand.

Read `plan/ml-webgpu-inference.md` first — it has the full research. The decisions
below are settled; **do not re-litigate them**:

## Settled architecture (do not deviate without strong new evidence)

- **Engine: `onnxruntime-web` with `executionProviders: ['webgpu','wasm']`.** NOT
  a Rust ONNX runtime. Researched: wonnx is archived/dead; burn needs build-time
  codegen (no on-demand models); tract is CPU-only; `ort` on wasm just wraps
  onnxruntime-web; the app renders with glow/WebGL2 (no `wgpu::Device` to reuse).
  onnxruntime-web is the best in-browser WebGPU ONNX engine and partitions the
  graph so unsupported ops (EQTransformer's LSTM) fall back to the wasm/CPU EP in
  the same session.
- **Location: inside the Pyodide worker** (`web/bridge_worker.ts`), driven by
  `bridge.py` via `import js`. The bridge is strictly one-way Rust→worker with no
  Python→Rust callback, so inference must live where `bridge.py` can reach it —
  the worker. Do NOT add a reverse channel and do NOT move inference to
  main-thread Rust.
- **Cross-origin isolation is already satisfied** (the app runs multithreaded
  wasm; `crossOriginIsolated === true`). onnxruntime-web's WebGPU + threaded wasm
  will work. Constraints: (1) bundle ORT's `.wasm`/`.mjs`/`.jsep` artifacts
  **same-origin** and set `ort.env.wasm.wasmPaths` to them (COEP credentialless
  blocks cross-origin CDN fetches for these); (2) **do NOT enable ORT's
  proxy-worker** — it is incompatible with the WebGPU EP and we are already in a
  worker.
- **SeisBench source cannot be imported in Pyodide** (hard `torch>=1.10.0` dep;
  models are `torch.nn.Module`; pyrocko's own picker just wraps it). Keep a
  faithful numpy pre/post port, guarded against drift by three MANDATORY measures:
  (1) **vendor verbatim** — copy the exact pre/post functions from seisbench
  source (`annotate_batch_pre`/`annotate_batch_post` per model, `_cut_fragments`,
  `stack_windows`/blend, `picks_from_annotations`, `trigger_onset`), swapping only
  torch ops for numpy, citing the source file+function above each; do NOT
  re-derive from scratch; (2) **pin the version** — a `SEISBENCH_VERSION` constant
  used by both the port and the exporter; (3) **per-model committed parity test**
  (see Task 6).
- **`embed-data` cargo feature, ON by default**, gates whether the model `.onnx`
  weights are bundled (offline) or fetched on demand via `snuffler-cors-proxy`
  (leaner build). The ORT engine artifacts and the tiny manifest are ALWAYS
  bundled. See Task 5.

## Tasks

### 1. Build step — export the model zoo

Extend `pybuild/export_phasenet.py` into `pybuild/export_seisbench_models.py`
(keep it runnable via `uv run --with seisbench --with torch --with onnx --with
onnxruntime`). For a curated set — start with `PhaseNet`, `EQTransformer`, `GPD`
(pick sensible pretrained weights, e.g. `instance`/`original`/`stead`) — export
each to a raw **`.onnx`** into `web/assets/models/<model>_<weights>.onnx`. Drop the
`.npz` format entirely. Emit **`web/assets/seisbench_models.json`**: a manifest
with, per model, everything the runtime needs to reproduce seisbench exactly —
`sampling_rate`, `in_samples`, `component_order` (input channel order),
`labels`/output-channel semantics (PhaseNet `PSN`; EQTransformer detection+P+S
with its own thresholds), `norm` type + `norm_amp_per_comp` + `norm_detrend`,
`overlap`, `blinding`, and the calibrated `default_args` thresholds. Validate each
export against onnxruntime on the host before writing (max|diff| < 1e-3 on random
input). Reuse the exact seisbench spec already captured in
`web/seisbench_pick.py`'s comments for PhaseNet, and read the seisbench source
(`seisbench/models/{phasenet,eqtransformer,gpd,base}.py`) for the others'
pre/post. Record the exact `SEISBENCH_VERSION` the export was taken from. The
manifest is always bundled; the `.onnx` files are staged into `dist/` only under
the `embed-data` feature (Task 5).

### 2. Runtime JS — onnxruntime-web in the worker

Add `onnxruntime-web` to `package.json` (bun). In `web/bridge_worker.ts`, add a
lazy-initialized helper exposed to Pyodide as
`globalThis.__snufflerOrtInfer(modelUrl: string, input: Float32Array, dims:
number[]) => Promise<{data: Float32Array, dims: number[]}>` that:
- on first use, dynamic-imports `onnxruntime-web/webgpu`, sets
  `ort.env.wasm.wasmPaths` to the bundled same-origin artifact dir, and (if
  needed) `ort.env.wasm.numThreads`;
- caches one `InferenceSession` per `modelUrl`, created with
  `executionProviders: ['webgpu','wasm']`;
- runs inference (build an `ort.Tensor('float32', input, dims)` for the model's
  input name; return the first output's data+dims);
- surfaces errors as rejected promises.
Bundle the ORT artifacts via `index.html` `copy-file` (or `copy-dir`) so they are
same-origin, matching how `bridge.py`/`phasenet.npz` are staged today. Confirm the
bun build (`bun build web/bridge_worker.ts …`, see `Trunk.toml`) inlines or
correctly resolves the ORT import; keep the worker bundle lean (lazy-load ORT only
when a pick runs).

### 3. Runtime Python — replace the interpreter

Create `web/seisbench_infer.py` replacing `web/onnx_infer.py` +
`web/seisbench_pick.py`. It loads `seisbench_models.json`, does faithful per-model
windowing/normalization/blinding/blending and pick extraction (port from
seisbench; reuse the existing `trigger_onset`, `extract_picks`, windowing from
`seisbench_pick.py`, generalized to read params from the manifest), and calls
`await js.__snufflerOrtInfer(model_url, windows_f32, dims)` for the forward pass
(pass numpy→JS via `pyodide` typed-array interop; convert the result back to
numpy). In `web/bridge.py`: rename `_ensure_phasenet` →
`_ensure_seisbench_model(name, weights)` (fetch the chosen `.onnx` URL, cache per
key), generalize `_seisbench_pick_result` to take model/weights from the request,
and keep the ZNE grouping (`_seisbench_groups`). Update `web/bridge_worker.ts`
staging: stage `seisbench_infer.py` (+ the manifest fetch) instead of
`onnx_infer.py`/`seisbench_pick.py`; keep the cached model in the `_keep` reload
tuple. Delete `web/onnx_infer.py`, `web/seisbench_pick.py`,
`web/assets/phasenet.npz`, and their `index.html`/`taskfile.yaml`/mypy `files`
references.

### 4. Proto + UI — model selection

Add a `Seisbench` message to `SnufflingParams` in
`proto/snuffler/bridge/v1/snuffling.proto` (fields: `model` name, `weights`,
optional `p_threshold`/`s_threshold` overrides). Regenerate (`buf generate`).
Thread it through `SnufflingParams` in `src/snufflings/mod.rs` and give
`src/snufflings/seisbench.rs` **model + weights dropdowns** (populate from a static
list matching the exported set) plus optional threshold sliders. `bridge.py` reads
the params.

### 5. `embed-data` feature (default ON) — bundle vs fetch-on-demand

Add a cargo feature **`embed-data`** to `snuffler-web`, included in the crate's
**default** features. It gates only the heavy model `.onnx` weights; the ORT
engine artifacts and `seisbench_models.json` manifest are always bundled.

- **On (default):** stage `web/assets/models/*.onnx` into `dist/`; the app fetches
  models from local `assets/models/…`.
- **Off:** don't bundle the `.onnx`; the app fetches the chosen model on demand
  from the seisbench model repository through `snuffler-cors-proxy`
  (`http://127.0.0.1:17684/cors?url=…`, as FDSN already does), caching per model.

Wire it across the three build systems like the existing `threads` knob:
- **cargo:** the app reads `cfg!(feature = "embed-data")` and puts the resolved
  model **base URL** (local vs remote) into the picker request DTO — no hardcoded
  URLs via side channels.
- **Trunk:** add a conditional post_build copy hook (mirror the existing py-mt
  conditional-copy hook in `Trunk.toml`) that stages the `.onnx` files only when
  an env var such as `SNUFFLER_EMBED_DATA` is set.
- **mise/taskfile:** the default `serve`/`build` tasks build with the feature and
  set `SNUFFLER_EMBED_DATA=1` together (as they already pair `threads` with
  `CARGO_UNSTABLE_BUILD_STD`/`TRUNK_BUILD_NO_SRI`); document a lean variant that
  builds `--no-default-features` (re-adding needed defaults) and leaves it unset.

### 6. Validation (required, do not skip)

- **Export/interpreter parity (host):** each exported `.onnx` matches onnxruntime
  on random input (< 1e-3).
- **Pipeline parity (host):** promote `scratchpad/seisbench_validate.py` into a
  committed host test that, for EACH shipped model, runs real `seisbench` (pinned
  version) `model.classify()` on a test stream and asserts our
  `seisbench_infer`-path picks match to **≤1 sample** and the P/S probability
  curve correlation > 0.999. This is the anti-drift guard replacing "reuse the
  source" — **wire it into the taskfile so CI runs it** (it needs
  seisbench+torch, so gate it behind a dedicated task, not the default `check`).
- **Live browser (Playwright, the app is served at http://127.0.0.1:17683):** open
  Snufflings → SeisBench picker, select each model, Run over the loaded 6-station
  3-component data; confirm P/S markers appear at onsets and **0 console errors**.
  Verify via console/log that the **WebGPU EP is actually used** (ORT logs the EP),
  and that it still runs when WebGPU is unavailable (wasm fallback).

## Project rules (enforced)

- Run `task lint`, `task check:py`, `task check:ts`, `task check:threads`,
  `task lint:py`, and strict mypy — all must be green. wasm target is the real one
  (`cargo lint --target wasm32-unknown-unknown`).
- Protobuf messages are the ONLY DTOs (no JSON/Dict DTOs crossing the bridge).
- Faithfulness over cleverness: output must match SeisBench; never re-derive
  seismology you can copy from seisbench source. Keep comments explaining *why*.
- Commit only when asked; work on `main`; no `Co-Authored-By`/self-attribution
  trailers; lowercase-imperative Conventional-Commit subjects.
- Preserve existing comments; no decorative section-banner comments.

## Non-goals

- No Rust ONNX runtime, no reverse Python→Rust channel, no eframe backend switch
  to wgpu (all researched and rejected — see `plan/ml-webgpu-inference.md`).
- Training/fine-tuning in-browser. Inference only.
