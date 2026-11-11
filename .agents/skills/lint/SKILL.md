---
name: lint
description: >-
  How to lint, type-check, format, and test the snuffler-web repo. Use this
  BEFORE running any clippy/mypy/ruff/cargo-test command here — always go through
  the `task` runner (taskfile.yaml); ad-hoc invocations give wrong results.
---

# Linting & testing snuffler-web

This repo is a Cargo workspace (Rust/egui → wasm) plus a Python bridge
(`web/bridge.py`, runs in Pyodide) and protobuf schemas. **Always drive checks
through the `task` runner** — the tasks pin tool versions and pass the flags/deps
the bare tools need. Running the underlying tools by hand gives false results.

## Commands (run these, not the raw tools)

| Goal | Command |
| --- | --- |
| Lint everything (Rust all-features + toolkit boundaries + Python) | `task lint` |
| Lint across **all feature combinations** (CI-equivalent) + Python | `task lint:fc` |
| Lint & auto-fix Rust | `task lint:fix` (alias `task fix`) |
| Lint Python only (ruff) | `task lint:py` |
| Type-check (Rust + Python) | `task check` |
| Type-check Python only (mypy) | `task check:py` |
| Format all (Rust, Python, JS, proto) | `task format` |
| Test (workspace) | `task test` · all feature combos: `task test:fc` · doctests: `task test:doc` |
| List every task | `task --list-all` |

The pre-commit gate is **`task lint:fc` + `task check:py` + `task test`** (plus
`task format` to fix formatting). `task lint:fc` covers both Rust (every feature
combination of `gf` / `embed-data`), the threaded wasm check,
the toolkit boundaries, and the Python ruff lint.

## Why not the raw tools (the traps)

- **Do NOT run `mypy`/`uvx mypy` directly.** The Python type stubs need
  `types-protobuf` + `numpy` and the generated `*_pb2` modules; `task check:py`
  installs them via `uvx --with …` and runs `buf generate` first. A bare
  `uvx mypy web/bridge.py` reports ~200 bogus `no-any-unimported` errors from the
  generated protobuf stubs. Use `task check:py`.
- **Do NOT run a single `cargo clippy`** and assume it's clean. The gate is the
  whole feature matrix (`cargo fc lint`, via `task lint:fc`) **and** the wasm
  target — a lint can pass on the host target but fail on `wasm32` or on a
  non-default feature set. clippy config lives in `Cargo.toml [lints]`
  (edition 2024, `pedantic` + restriction lints all `deny`,
  `allow_attributes_without_reason`), so every `#[allow]` needs a `reason = "…"`.
- **Generated code:** the protobuf Python/Rust stubs come from `buf generate`
  (a Trunk `pre_build` hook and `check:py` run it). Don't hand-edit `*_pb2*`.
- **ruff** is the Python linter (`task lint:py`) and formatter (`task format:py`),
  pinned to 0.14.8; rules are in `pyproject.toml [tool.ruff.lint]`. `B009`/`B010`
  are intentionally ignored — this bridge uses `getattr`/`setattr` with constant
  names on purpose (scipy/pyrocko stub gaps + custom attrs on `sys.modules` stub
  objects), with explicit type annotations so mypy stays correct.

## After changing code

1. `task format` — fix formatting.
2. `task lint:fc` — Rust feature matrix + ruff.
3. `task check:py` — mypy.
4. `task test` — tests.

Everything must report 0 errors / 0 warnings before committing.
