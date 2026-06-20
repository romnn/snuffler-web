- Pyodide in a Web Worker
  Move Pyodide off the main thread so heavy Python (filters, FFT, future synthetics) never freezes the egui UI. No cross-origin isolation needed. Requires reworking the bridge.js ↔ Rust path to async message-passing with transferable buffers. Pure responsiveness/UX win.

- Cross-origin-isolated threads
  Add a coi-serviceworker shim for SharedArrayBuffer, then pursue real multithreading (OpenMP in the C exts and/or Rust worker threads). Large and experimental — needs a custom pthread-enabled Pyodide build. Marginal compute payoff for a browser app.
