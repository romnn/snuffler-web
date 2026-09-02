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

- **End a borrow with a scope, never with `drop(guard)`.** The bridge's state is a
  `RefCell` behind an `Rc`, so a `borrow()`/`borrow_mut()` guard held one line too
  long is not a warning — it is a panic at runtime, on whichever async result
  happens to land in that window. Write the region the borrow is allowed to live
  in and let it end there:

  ```rust
  {
      let mut i = inner.borrow_mut();
      i.scene = Some(scene);
  }                       // the borrow ends here, visibly
  repaint.request_repaint();

  let svg = {             // or: bind the value the borrow produced
      let Some(view) = bridge.view() else { return };
      render_svg(&view)
  };
  ```

  `drop(i)` does the same thing today and silently stops doing it tomorrow: it is
  one statement among many, so a later edit that adds a line after it — or moves
  the call inside a branch — extends the borrow with nothing to say so. A block
  makes the lifetime structural, the compiler enforces it, and a reader sees the
  extent without tracing statements. This applies to every guard, not just
  `RefCell`: `MutexGuard`, `Ref`/`RefMut`, and anything else whose whole job is to
  be released.

- **Errors are typed in library code and `eyre` in tests.** Two rules, one seam
  between them:
  - **Non-test code returns a `thiserror` enum.** Every fallible function in a
    crate's real code carries a `#[derive(thiserror::Error, Debug)]` type with one
    variant per failure mode (`#[from]` for wrapped sources). Never
    `Result<_, String>`, never a stringly-typed variant, and never `eyre`/`anyhow`
    in a library signature: a `String` cannot be matched on, so a caller cannot
    recover selectively and the compiler cannot tell it a new failure mode exists.
    The bridge is the worked example. A failed call is a
    `snuffler_web_proto::CallError` — `Transport` (the worker never answered),
    `Remote` (pyrocko raised, and the traceback rides along), `Decode`, `Empty`,
    or `UnexpectedPayload` (the schema and the generated ends have drifted) — and
    `snuffler_web_core::bridge::BridgeError` adds what the proto crate cannot see:
    a rejected DOM call, a malformed JS value, `SeedLink`, an unknown example
    slug. Keep those distinctions when you add a failure mode; they are the whole
    reason the type exists, since a status line can only ever show one sentence
    but the app can act on the variant. `BridgeError` deliberately holds no
    `JsValue`: it is stored in the activity ledger (`JobError::Bridge`,
    `RuntimeState::Failed`) and in the bridge's `diagnostic` slot, and read by
    the egui frontend — all of which build on the host. Stringify the JS
    exception at the boundary instead.
  - **Tests use `color-eyre`, and only where they can actually fail.** A test that
    reaches for a fixture returns `eyre::Result<()>` and propagates with `?`,
    `bail!`, `ensure!`, or `let ... else { bail!(…) }`. A test whose only failure
    is an assertion stays a plain `fn` with `assert!`/`assert_eq!` — do not wrap an
    infallible test in a `Result` it never uses.

  **Always spell the type `eyre::Result`.** Import the module
  (`use color_eyre::eyre::{self, bail, ensure};`) and write `-> eyre::Result<()>`.
  Never import `Result` directly, never alias it to something else, and never
  shadow `std::result::Result` — at a glance the reader has to see which `Result`
  a signature means without checking the imports.

  This is not a style preference: the workspace **denies** `clippy::unwrap_used`,
  `expect_used`, and `panic`. A test module that opens
  `#![expect(clippy::unwrap_used, …)]` to get around them is a hole in the very
  policy the lints exist to enforce — the fix is an error that says what was
  missing, not a suppression.
