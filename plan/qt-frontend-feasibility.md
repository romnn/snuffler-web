# Qt frontend in the browser — feasibility, the one viable path, and status

> Status: **wasm route investigated and rejected; shipped a working alternative.**
> Qt-to-wasm is not viable in a normal work session, so the Qt frontend instead
> streams the real desktop snuffler to the browser (see [`../qt/`](../qt/)). This
> doc is the rationale. The Rust/egui frontend is unaffected. Captured 2026-06-19.

## TL;DR

Compiling snuffler's **original PyQt5 GUI to WebAssembly** is effectively a dead end:
no official PySide6/PyQt wasm exists, the ABIs don't line up, and the only viable
wasm path (Qt-in-Pyodide) is a **2–4 month, research-grade, perpetually-fragile**
effort with a realistic **~50–60 % feature ceiling**. This document records that.

**So the shipped Qt frontend does NOT use wasm.** It runs the *real, unmodified*
PyQt5 snuffler server-side and streams it to the browser over noVNC —
**100 % feature-complete**, working today, coexisting with the egui frontend
(`task qt:build` / `task qt:serve`). The wasm analysis below is the rationale.

## What is genuinely impossible (don't try these)

- **Standalone PySide6 for WebAssembly.** No official support; the tracking issue
  [PYSIDE-962](https://bugreports.qt.io/browse/PYSIDE-962) is still open (mid-2026).
  No wasm wheel, no wasm CPython, no `pyside6-deploy --target=wasm`. Qt's own 2022
  dev notes record the attempt stalling on toolchain/ABI mismatch; the 2024 notes
  drop the topic entirely.
- **PyQt5/PyQt6 for WebAssembly from Riverbank.** No wasm support, ever. Riverbank
  lists only Windows/macOS/Linux/Android/iOS.
- **Reusing our Pyodide numpy/scipy wheels under a Qt-built CPython.** Emscripten
  gives **no cross-version ABI stability** (PEP 783) and the wheel tag is
  version-pinned (PEP 776). Qt's pinned emscripten (6.8→3.1.56, 6.9→3.1.70,
  6.10/6.11→4.0.7) never coincides with Pyodide's (0.27→3.1.58, 0.28→4.0.9), and
  even near-matches differ in build flags (wasm-exceptions, BIGINT, pthreads). A
  Qt-wasm build and a separate Python-wasm runtime **cannot share one module**.
- **scipy on an arbitrary new emscripten ABI.** Needs an OpenBLAS + Fortran-to-wasm
  port; only Pyodide (and emscripten-forge, which ships no Qt) has done it.

## The one viable path: Qt **inside** Pyodide

Because we already use Pyodide, the right question is not "PySide6 on wasm" but
"**Qt inside my Pyodide**" — one runtime, one emscripten ABI, hosting Qt **and**
numpy/scipy/pyrocko together. There is a real, working proof of concept:

- **[JarrettSJohnson/pyodide-with-pyqt6](https://github.com/JarrettSJohnson/pyodide-with-pyqt6)**
  (pushed 2026-03-08): statically links **Qt 6.10.2 + PyQt6 6.10.2** into a
  **from-source Pyodide 0.29.3 (CPython 3.13.2, Emscripten 4.0.9)**. PyQt6 modules
  are registered via `PyImport_AppendInittab`; Qt renders into a `<div>`. ~33 MB
  wasm. **Important caveat:** it is a 5-star, single-author PoC whose smoke test
  only verifies *module import in Node* — interactive widget rendering in a real
  browser is **unproven**. It also builds with `--disable QtNetwork QtPrintSupport
  QtOpenGL` and `-no-feature-thread` — exactly the modules snuffler needs.

### Exact toolchain (everything on ONE emscripten/ABI)

- Emscripten **4.0.9** (Pyodide 0.29.3's pinned emsdk) — used for Qt, PyQt6,
  Pyodide, *and* a rebuilt pyrocko (flags must match: `-fwasm-exceptions`, PIC,
  longjmp).
- **Qt 6.10.2** for wasm-emscripten, `-static`, subset `qtbase + qtsvg`, **plus**
  re-enabled `QtNetwork` + `QtPrintSupport`.
- **PyQt6 6.10.2 + sip 6.15.1 + PyQt6-sip 13.11.0** (Riverbank, **GPLv3** — a
  licensing constraint to accept) cross-built against the wasm Qt.
- **Pyodide 0.29.3 / CPython 3.13.2** built from source.
- pyrocko `signal_ext` + `mseed_ext` (+ libmseed) **rebuilt for cp313/emsdk-4.0.9**
  — a wholesale ABI bump from our current chain (Pyodide 0.27.2 / CPython 3.12 /
  emsdk 3.1.58). numpy/scipy/matplotlib come from the Pyodide package set.
- Build cost: **~10 GB, ~40 min** first build (Qt-from-source dominates), and it is
  documented as "really fragile" by the Pyodide maintainers (pyodide#1933).

## Why even this path yields only a partial snuffler

Confirmed against our actual checkout (`sources/pyrocko/src`):

| Blocker | Where | Consequence |
|---|---|---|
| Hardcoded **PyQt5**, imports `QtNetwork`/`QtPrintSupport` at module top | `gui/qt_compat.py:38-45` | Won't even import on the PoC; needs a PyQt5→PyQt6 port |
| Viewer subclasses **QOpenGLWidget** | `gui/snuffler/pile_viewer.py:4512` | QOpenGLWidget unsupported in Qt-wasm — but `use_opengl` defaults off (`:4554`), so the QPainter raster path can be forced |
| Blocking **`app.exec_()`** + nested `QDialog.exec()` | `snuffler.py:144`; `pile_viewer.py:1286,2854`; `snuffling.py:422`; `marker_editor.py:312` | Must rewrite event loop; nested exec needs Asyncify (slow) or JSPI (Chrome-only) |
| Synchronous **multi-file/dir `QFileDialog`** | `pile_viewer.py:1654-2868` | Rewrite to async single-file `getOpenFileContent` / download-only save |
| **QtWebEngine** map snuffling | `gui/util.py:1272` | Chromium has no wasm target — feature must be dropped |
| **QThread** live acquisition (SeedLink/serial/USB) | `gui/snuffler/snuffler_app.py:41` | Threads off by default — live streaming out of scope |
| **QtPrintSupport** printing | `marker_editor.py:309-311` | Printing unsupported in wasm |

**Realistic ceiling (~50–60 %):** static file/URL waveform viewing, the raster
pile-viewer canvas, markers/picking, numpy/scipy/pyrocko filtering, most non-network
snufflings, SVG. **Lost:** live acquisition, the map, printing, multi-file/dir open,
persistent local files (MEMFS wiped on reload), reliable cross-browser modal dialogs.
Bundle is **tens of MB** vs the egui build's ~3.7 MB — so Qt mode is inherently an
opt-in "heavy" mode, which fits the side-by-side requirement.

## Effort estimate

- Foundational build (reproduce PoC, re-enable modules, rebuild pyrocko C-exts for
  the new ABI, one Pyodide hosting Qt + scientific stack): **2–4 weeks**, fragile.
- PyQt5→PyQt6 port of pyrocko's GUI (a large codebase we don't own): **3–6 weeks**.
- wasm adaptation (event loop, async I/O, drop WebEngine/print/threads): **3–6 weeks**.
- **Total ≈ 2–4 months** + ongoing maintenance keeping Qt/PyQt6/Pyodide/emscripten
  version-locked, on a stack with no upstream support.

## Recommended next step (spike-first)

1. **Spike (~1 week):** reproduce the PoC build, re-enable `QtNetwork`/`QtPrintSupport`,
   and get a *trivial* `QPainter` widget that both **renders and accepts mouse input
   in a real browser**. If interactive raster rendering doesn't work, **stop** —
   everything downstream depends on it and the PoC never proved it.
2. Only if the spike succeeds, budget 2–4 months and scope to **static waveform
   viewing + markers + filtering**, dropping acquisition/map/print from the outset.

This wasm path is **not** what shipped. The working Qt frontend in [`../qt/`](../qt/)
streams the real desktop snuffler instead — full features, no port, no wasm.

## Sources

PYSIDE-962; doc.qt.io/qt-6/wasm.html; qtforpython considerations.html; Qt for Python
Dev Notes 2022/2024 (wiki.qt.io); pyodide#1933, pyodide-recipes#183;
pyodide.org ABI docs (0.27/0.28); PEP 776; PEP 783;
github.com/JarrettSJohnson/pyodide-with-pyqt6; github.com/viur-framework/pyodide-with-pyqt5 (archived 2020).
