# Qt frontend track — the original PyQt5 snuffler

This directory owns the real, unmodified PyQt5 snuffler environment.
It can run natively on the developer's desktop or in a fully independent browser
frontend that renders on a headless X server and streams the GUI over **noVNC**.

Neither mode touches the egui frontend.

## Run natively

The locked Python 3.12 project uses the same pinned Pyrocko source checkout as
the parity oracle.
`uv run` creates and updates `qt/.venv` automatically:

```sh
task snuffler
task snuffler -- example.mseed
task snuffler:example EXAMPLE=kilauea-2018-07-15-caldera-collapse
```

An explicit `uv sync --project qt` is optional.
The `parity` dependency group adds pytest and protobuf only when the oracle tests
need them.
Repository example directories also contain application metadata, an event
catalogue, and StationXML files, so pass their slug to `snuffler:example` rather
than passing the whole directory as waveform input.
On macOS, add `-- --no-opengl` to either Snuffler task if Qt's OpenGL paint warning
causes visible rendering problems.

## Why streaming, not WebAssembly

Qt-for-Python (PySide6/PyQt) **cannot be compiled to WebAssembly** in any usable
form — see [`../plan/qt-frontend-feasibility.md`](../plan/qt-frontend-feasibility.md)
for the full, sourced investigation (no official PySide6 wasm, ABI mismatches, a
2–4 month research-grade port with a ~50–60% feature ceiling). Streaming the real
desktop app sidesteps all of that and gives **every** snuffler feature, today.

## Run in a browser

Requires Docker.

```sh
task qt:build    # build the image (real PyQt5 snuffler + numpy/scipy/pyrocko + noVNC)
task qt:serve    # run it
# open http://127.0.0.1:6080/vnc.html?autoconnect=true&resize=remote
```

(6080 is the conventional noVNC port; change the mapping in `taskfile.yaml` if it's taken.)

`task qt:serve` launches snuffler on a synthetic demo dataset (same noise + travelling
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
