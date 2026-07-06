# What the wasm port genuinely cannot do that Qt snuffler can

> Status: **feature parity reached to the platform limit.** Every Qt-snuffler
> feature that is pure computation or curated I/O is implemented in the
> Rust/egui + Pyodide port — including the deep-learning `seisbench_picker`,
> which is now shipped (PhaseNet exported to ONNX, run in a pure-numpy
> interpreter in Pyodide; see [`../memory`](../) notes and
> `web/onnx_infer.py`). What remains are the true browser-sandbox boundaries.
> Captured 2026-07-05.

## TL;DR

The remaining gaps are not missing features — they are the boundary the browser
sandbox exists to enforce: **raw network sockets, silent/arbitrary local files,
subprocesses, and arbitrary native-dependency plugins.** Everything that is pure
computation or user-granted I/O is reachable, which is why the picker fell.

The limits sort into four buckets:

1. **Sandbox-hard** — no in-browser workaround at all.
2. **Needs-a-server** — possible in principle, but only with an external
   helper/proxy, so not "just the browser."
3. **Chromium-only** — works, but degrades in Firefox/Safari.
4. **Soft** — present but slower.

---

## 1. Genuinely impossible in a browser (no workaround without leaving it)

- **Live/real-time acquisition over raw network sockets.** Snuffler's online mode
  connects to a **SeedLink** server (`slink://…`) or a ringserver for streaming.
  Browsers have no raw TCP; the only socket is WebSocket, which a raw SeedLink
  server does not speak. Internet live-streaming is out unless someone runs a
  WebSocket↔SeedLink proxy (→ bucket 2).
  - *Nuance:* a **locally attached** serial/USB digitizer (EDL/usbserial
    acquisition) is technically reachable via the Chromium **Web Serial / WebUSB**
    APIs (user-permission, Chromium-only) — not implemented, but not
    fundamentally impossible. The raw *network* case is the hard wall.
- **Silent / arbitrary local filesystem access and CLI invocation.** Native
  snuffler is `snuffle /data/*.mseed`: it reads whatever paths you pass, watches
  them via inotify, and writes markers back to any path. In-browser, *every* file
  and directory needs an explicit user pick gesture; there is no CLI entry, no
  silent read of `/some/path`, and no silent write-back (saving requires a
  save-picker). "Watching" is polling, not inotify.
- **Spawning external processes / OS shell-outs.** Snufflings or workflows that
  call another program (`fomosto`, a system command, launching an external
  viewer) cannot run in the sandbox.
- **Arbitrary third-party snufflings with native or GUI dependencies.** Qt
  snuffler's real extensibility is: drop any `.py` in `~/.snufflings/` importing
  *anything* (a C-extension package, torch, GDAL, cartopy) and even building a
  PyQt panel. In-browser a snuffling can only use packages present in the custom
  Pyodide build (pure-Python or emscripten-compiled wheels) and cannot build
  native/Qt UI. The web port instead **reimplements the built-in snufflings** in
  `bridge.py`; it cannot load an unknown one off disk. GPU/CUDA is likewise
  absent.
- **True 64-bit memory scale.** wasm32 has a single ~4 GB heap. A native 64-bit
  process holds far larger piles / longer continuous records than the browser can.

## 2. Possible, but only with an external server/proxy

- **FDSN / HTTP endpoints that do not send CORS headers.** The browser can only
  fetch cross-origin resources that opt in via CORS, so the app ships a curated
  list of CORS-enabled datacenters. Native Qt hits *any* host; reaching the
  non-CORS ones needs a proxy.
- **SeedLink**, as above — only via a WebSocket bridge.

## 3. Chromium/Edge-only (degraded in Firefox & Safari)

Everything built on the **File System Access API**: directory mounting, local
Green's-function stores, watch-files, reload-modified-files, and re-readable file
handles. Firefox/Safari fall back to one-shot single-file upload — no directory
mount, no re-read.

## 4. Soft limits — slower, not absent

Heavy compute runs on CPU: the numpy ONNX interpreter vs native torch, and
single-threaded generic OpenBLAS (SIMD/OpenMP help, but there is no threaded BLAS
or GPU). Functionally present, just slower on big jobs.

---

## Honest caveat on the deep-learning picker

What shipped is **PhaseNet `instance`, pre-exported to ONNX** — bit-faithful picks
for *that* model (validated against onnxruntime to 1.2e-7 and against seisbench's
own `classify()` to ≤1 sample). Qt's `seisbench_picker` lets you choose the whole
model zoo (EQTransformer, GPD, other weights, custom checkpoints) and runs on GPU.
So it is the *same output for the shipped model*, but not arbitrary-model-on-demand
and not GPU-fast. Adding another model is now just "export it to ONNX and bundle
it" — the runtime path (`web/onnx_infer.py` + `web/seisbench_pick.py`) already
generalizes.

## Not gaps — already solved (to preempt the question)

These once looked like platform limits but are done: threaded compute
(pthreads/OpenMP/SIMD via the custom Pyodide 314 build), file/directory loading and
GF-store mounting (File System Access API), FDSN catalog/waveform/station fetch
(CORS-enabled endpoints), all matplotlib figures (rendered to PNG and blitted as
egui textures), and the deep-learning picker (ONNX→numpy in Pyodide). See the
per-subsystem memory notes for how each was closed.
