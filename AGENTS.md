# Rules

## Architecture

- **All numerical/scientific work lives in pyrocko (Python, in Pyodide).** The
  Rust/egui frontend ONLY renders — it never does actual computation. If you find
  yourself computing something in Rust, move it into `web/bridge.py`.

- **All cross-boundary communication is protobuf — no text, no JSON.** Every DTO
  exchanged between Rust ↔ `web/bridge.js`/`bridge_worker.js` ↔ `web/bridge.py`
  is a protobuf message serialized to bytes (schema in `proto/`). This is for
  performance (compact, zero-copy buffers for waveform data) and type safety
  (the same messages generate Rust structs + Python classes). Do not introduce
  `str`/JSON return values across the bridge.

- **The protobuf schema is internal — never keep wire compatibility.** `proto/`
  is a private boundary between parts of one artifact: the Rust/wasm client,
  `web/*.ts`, and `web/bridge.py` all generate from the same schema in the same
  build and ship together in a single deploy step. No third party speaks this
  schema, and protobuf bytes are never persisted (no localStorage/IndexedDB, no
  session files, no `eframe` state) — they only ever live in flight between the
  wasm module, the worker, and Pyodide. So a schema edit is a *source* change,
  not a wire change: freely rename, renumber, retype, or delete fields, and fix
  every generated call site in the same commit. Never add `reserved` ranges,
  `[deprecated = true]`, "kept for older bundles" fields, or a scalar field left
  beside its `repeated` successor — and never write compat shims in `bridge.py`
  that fall back from a new field to an old one. Reusing a retired field number
  is fine. There is no `buf breaking` check here, by design.

- **Errors are protobuf too — never raise across the JS boundary.** Every bridge
  response is wrapped in a `oneof { success | error }` envelope. `bridge.py`
  catches exceptions and returns the **error variant**; it must never let a
  Python exception propagate out to the JS caller. Rust converts the envelope
  into a `Result`. Why this matters: on the threaded build the WASM heap is a
  `SharedArrayBuffer`, and pyodide's conversion of a *raised* Python exception's
  traceback string to JS trips `TextDecoder` ("ArrayBufferView must not be
  shared") — masking the real error. Returning errors as proto **bytes** (like
  every other DTO) sidesteps that entirely and is type-safe end to end.

- **Never block the egui render loop — it is hot.** `update()` runs on the single
  browser main thread, every frame; ANY synchronous work there freezes the whole
  UI (pan, zoom, repaint) for its full duration. A frame must only draw the latest
  snapshot (plus a progress indicator if a job is running) — heavy work happens
  elsewhere. Two distinct cases, because they live in two SEPARATE wasm modules:
  - **Compute (filters, FFT, synthetics, IO, snufflings):** already off-thread —
    it runs in pyrocko inside the Pyodide web worker, dispatched with
    `spawn_local` + an async bridge call that `request_repaint()`s when the result
    lands. Keep new heavy compute on this path; never pull it onto the main thread.
  - **Rust-side work (e.g. the resvg high-DPI PNG/SVG export):** this runs in the
    egui app's OWN `wasm32-unknown-unknown` module (separate from Pyodide —
    different module, memory, and build; Pyodide's pthread/OpenMP/SIMD threads do
    **not** apply here). The app's own parallelism is a **rayon worker pool**
    (`wasm-bindgen-rayon`, the `threads` feature) — a general-purpose concurrency
    tool for any heavy Rust loop, not just the PNG export. So offload long Rust
    tasks: snapshot the inputs, then `rayon::spawn` (or a parallel iterator) onto
    the pool and `request_repaint()` when it lands, behind a progress indicator.
    The pool needs cross-origin isolation; when it is absent (`pool::ready()` is
    false) fall back to chunking across frames or running inline. Never call a
    multi-hundred-millisecond synchronous routine straight from `update()`.
