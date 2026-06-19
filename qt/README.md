# Qt frontend track — the original PyQt5 snuffler, in the browser

A **second, fully-independent** way to run snuffler in the browser, alongside the
Rust/egui frontend. This one runs the **real, unmodified PyQt5 snuffler** — so it
is **100% feature-complete** — by rendering it on a headless X server and streaming
the GUI to the browser over **noVNC**.

It does not touch the egui frontend: separate Docker image, separate port (8090),
separate commands. The two coexist.

## Why streaming, not WebAssembly

Qt-for-Python (PySide6/PyQt) **cannot be compiled to WebAssembly** in any usable
form — see [`../plan/qt-frontend-feasibility.md`](../plan/qt-frontend-feasibility.md)
for the full, sourced investigation (no official PySide6 wasm, ABI mismatches, a
2–4 month research-grade port with a ~50–60% feature ceiling). Streaming the real
desktop app sidesteps all of that and gives **every** snuffler feature, today.

## Run

Requires Docker.

```sh
mise run qt-build    # build the image (real PyQt5 snuffler + numpy/scipy/pyrocko + noVNC)
mise run qt-serve    # run it
# open http://127.0.0.1:6080/vnc.html?autoconnect=true&resize=remote
```

(6080 is the conventional noVNC port; change the mapping in `mise.toml` if it's taken.)

`qt-serve` launches snuffler on a synthetic demo dataset (same noise + travelling
event as the egui example). Load your own MiniSEED/SAC via snuffler's File menu
inside the streamed GUI.

## How it works

```
 Browser ──HTTP/WebSocket──> websockify+noVNC ──VNC──> x11vnc ──> Xvfb (:99)
                                                                    └─ openbox
                                                                    └─ snuffler (real PyQt5)
                                                                          └─ numpy/scipy/pyrocko
```

- `Dockerfile` — Ubuntu + system PyQt5/numpy/scipy/matplotlib + pyrocko (PyPI wheel)
  + Xvfb + x11vnc + noVNC.
- `entrypoint.sh` — starts Xvfb + openbox, generates demo data, launches snuffler,
  exposes it via x11vnc, serves noVNC/websockify on :8080 (mapped to host :8090).
- `make_demo.py` — writes the synthetic demo MiniSEED.

## Trade-offs

- Full fidelity and all features (it's the actual app), but it runs **server-side**:
  one container per session, server CPU/RAM, and latency over the wire — unlike the
  egui frontend which runs entirely client-side. Pick egui for a lightweight public
  page; pick this for full snuffler fidelity.
