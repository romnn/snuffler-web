# The original PyQt5 snuffler

This directory owns the environment for the real, unmodified PyQt5 snuffler.
It is the reference the parity tests in `tests/parity/` measure the web app
against, and it runs natively on the developer's desktop.

## Run natively

The locked Python 3.12 project uses the same pinned Pyrocko source checkout as
the parity oracle.
`uv run` creates and updates `qt/.venv` automatically:

```sh
task qt
task qt -- example.mseed
task qt:example EXAMPLE=kilauea-2018-07-15-caldera-collapse
```

An explicit `uv sync --project qt` is optional.
The `parity` dependency group adds pytest and protobuf only when the oracle tests
need them.
Repository example directories also contain application metadata, an event
catalogue, and StationXML files, so pass their slug to `qt:example` rather
than passing the whole directory as waveform input.
On macOS, add `-- --no-opengl` to either Snuffler task if Qt's OpenGL paint warning
causes visible rendering problems.

Qt-for-Python has no usable WebAssembly build, so this application cannot run in
the browser; see [ADR 2](../adr/0002-upstream-qt-is-a-reference.md).

## Drive it headless

`qt/driver.py` runs this snuffler without a display and takes commands, so an
agent can compare it with the web app step by step; see its `--help`.

```sh
task qt:headless EXAMPLE=kilauea-2018-07-15-caldera-collapse
uv run --project qt python qt/driver.py screenshot snuffler.png
```
