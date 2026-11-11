# 5. A toolkit-free core, so any frontend can be added

**Status:** accepted · **Date:** 2026-07-12

## Context

snuffler-web has one frontend, egui, but it is meant to get others: a DOM frontend
(leptos with tailwind) or one built on Zed's gpui are both wanted.
What makes a second frontend cheap is that all computation already lives in pyrocko behind
the protobuf bridge, so the Rust side only owns state and rendering.
That only stays true if the state and rendering code never start to depend on the toolkit
that happens to draw them today.
Before the split, the egui-free code was interleaved with egui-owned files and kept apart by
convention alone, and `Bridge` took an `&egui::Context` in some thirty-five methods just to
request a repaint.

## Decision

The workspace has two shared crates that know no UI toolkit, and one thin crate per frontend
on top of them.

- **`snuffler-web-core`** holds the bridge (transport, dispatch, caches, generation
  counters), `AppState` and its settings, the marker model and `:` command parsing, the time
  axis, SeedLink framing, snuffling configurations, and `Session`, which drains bridge results
  into state and state changes into bridge calls each frame.
- **`snuffler-web-render`** draws waveforms, waterfalls, axes and markers through the
  `DrawTarget` trait; `SvgTarget` is its own backend and `EguiTarget` lives in the egui crate.
- **A frontend crate** (today `snuffler-web-egui`) owns the chrome: menus, dialogs, forms,
  the control strip, the marker table and the snuffling panels, plus mapping its input events
  onto the shared code and its canvas onto `DrawTarget`.

The seams between them are `Session`, `Repaint` (a redraw request built from two
frontend-owned callbacks) and `DrawTarget`, and nothing higher.
Deliberately absent:

- no widget, dialog or view-model abstraction traits: the chrome is per toolkit by design,
  and sharing it would remove the reason to write another frontend;
- no message enum for state changes: egui mutates `AppState` directly, which is right for
  immediate mode, and a reactive frontend wraps `AppState` at its own edge (signals, stores)
  instead of the core growing them;
- no DTO layer: decoded prost messages are the model every frontend reads;
- no frontend crate before someone builds that frontend.

**The boundary is enforced, not encouraged.**
`task lint:boundaries` fails when the dependency tree of `snuffler-web-core`,
`snuffler-web-render` or `accesskit-dom` contains any UI toolkit or windowing/GPU stack
(egui and its crates, eframe, epaint, winit, glow, glutin, wgpu, gpui, leptos), on every
target.
It runs in `task lint` and `task lint:fc`, and so in CI; the colour and geometry types
`ecolor` and `emath` stay allowed.
Cargo's ban on dependency cycles keeps the direction: a frontend may depend on core, never the
reverse.

## Consequences

- A new frontend reuses the bridge, the state, `Session` and the whole waveform renderer, the
  hardest part of the UI, and rewrites only its chrome (in egui roughly 2,500–3,000 lines).
- Pan, zoom and pick handling still reads egui input types (`handle_interaction` and
  `handle_keyboard` in `snuffler-web-egui`).
  The math is toolkit-independent; it moves into core as plain functions over position,
  modifiers and wheel delta when the second frontend needs it, not before.
- The bridge's transport is browser-only: it talks to the Pyodide worker and is gated on
  `wasm32`.
  A frontend in the browser (leptos, or gpui's web target) reuses it as is.
  A native desktop frontend additionally needs a native transport that runs the same
  `web/bridge.py` against a host CPython; the protobuf contract and the Python side stay
  unchanged.
- Adding a dependency to core or render means checking that it is not a toolkit in disguise;
  the lint names the crate and the offending dependency when it is.

## References

- `crates/snuffler-web-core/src/session.rs`, `crates/snuffler-web-core/src/bridge/mod.rs`
  (`Repaint`)
- `crates/snuffler-web-render/src/draw.rs` (`DrawTarget`, `SvgTarget`)
- `crates/snuffler-web-egui/src/view/draw.rs` (`EguiTarget`)
- `taskfile.yaml` (`lint:boundaries`)
