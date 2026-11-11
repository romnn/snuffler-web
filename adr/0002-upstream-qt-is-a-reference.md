# 2. Upstream Qt snuffler is a reference, not a frontend

**Status:** accepted · **Date:** 2026-11-11 · **Supersedes:** the noVNC `qt` image and the
browser-Qt investigation of 2026-06-19

## Context

snuffler-web exists to put pyrocko's snuffler in the browser, so the obvious question is why
it does not just ship the original PyQt5 GUI.
Two routes were tried.

**Compiling the Qt GUI to WebAssembly is not viable.**
There is no PySide6 or PyQt build for WebAssembly from Qt or Riverbank.
Emscripten gives no cross-version ABI stability, so a Qt-wasm build and the Pyodide runtime
that carries numpy, scipy and pyrocko cannot share one module.
The only working proof of concept (Qt 6 and PyQt6 linked into a from-source Pyodide) is a
single-author build that never proved interactive rendering in a browser, and drops
`QtNetwork`, `QtPrintSupport` and threads, which snuffler needs.
On top of that, snuffler's GUI would need a PyQt5-to-PyQt6 port, an event loop without
blocking `exec_()` and nested modal dialogs, and async file dialogs; live acquisition, the
QtWebEngine map and printing would be lost outright.
The estimate was two to four months for roughly half of snuffler's features, on a stack with
no upstream support and a bundle tens of megabytes larger than the egui app.

**Streaming the real desktop app over noVNC works but is the wrong product.**
It runs server-side, one container per session, with server CPU and latency, while the point
of snuffler-web is that it runs entirely in the browser.
Its only remaining use was letting someone compare the two apps, and an agent does that far
better against the native application than through a remote framebuffer.

## Decision

snuffler-web ships one frontend: the Rust/egui app driving real pyrocko in Pyodide.

The unmodified upstream PyQt5 snuffler is kept only as the **reference** the web app is
measured against.
It runs natively, from the locked Python 3.12 project in `qt/` at the same pinned pyrocko
commit as the web build:

- the parity suite in `tests/parity/` drives it offscreen as its oracle;
- `qt/driver.py` runs it headless and takes named commands, so an agent can take the same
  steps in both apps and compare state and screenshots.

There is no container image, compose service or published package for it.

## Consequences

- Upstream behaviour, including its quirks, is the specification; a difference from it is a
  defect unless `tests/parity/known_gaps.toml` justifies it.
- Comparing the apps needs a desktop-class Python environment with PyQt5 (`uv run --project
  qt`), not Docker.
  Nothing about it is deployed.
- Features that only a native process can offer (raw sockets, arbitrary local files,
  subprocesses, third-party snufflings with native dependencies) stay out of the web app;
  `known_gaps.toml` records each one with its `sandbox-hard`, `needs-a-server`,
  `chromium-only` or `soft` bucket.
- If Qt or PySide ever ships a supported WebAssembly target that shares Pyodide's ABI, the
  wasm route is worth a fresh spike; nothing short of that changes the estimate above.

## References

- `qt/driver.py`, `qt/pyproject.toml`
- `tests/parity/qt_oracle.py`, `tests/parity/known_gaps.toml`
