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

- **Errors are protobuf too — never raise across the JS boundary.** Every bridge
  response is wrapped in a `oneof { success | error }` envelope. `bridge.py`
  catches exceptions and returns the **error variant**; it must never let a
  Python exception propagate out to the JS caller. Rust converts the envelope
  into a `Result`. Why this matters: on the threaded build the WASM heap is a
  `SharedArrayBuffer`, and pyodide's conversion of a *raised* Python exception's
  traceback string to JS trips `TextDecoder` ("ArrayBufferView must not be
  shared") — masking the real error. Returning errors as proto **bytes** (like
  every other DTO) sidesteps that entirely and is type-safe end to end.
