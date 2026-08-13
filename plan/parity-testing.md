# Parity testing: proving the web port behaves like Qt snuffler

> Status: **proposal, nothing built yet.** Drafted 2026-08-13 against pyrocko
> `64ae99fb` (the `PYROCKO_REF` pin in `mise.toml`). Supersedes the assumption
> that comparing the two frontends requires driving the noVNC container.

## TL;DR

Comparing the two frontends does **not** mean comparing two GUIs. Both sides are
importable Python with a clean "what would be drawn" seam, so the bulk of the
comparison runs in **one CPython process, headless, in milliseconds** — no
browser, no X server, no VNC, no Docker.

Three tiers, in descending value per unit of effort:

1. **Oracle parity** — call `PileViewerMain.prepare_cutout2()` and
   `bridge._get_view_data()` on the same inputs, diff the numbers. Catches the
   failures that matter and that nothing else can see.
2. **Feature-inventory diff** — extract the complete command surface from both
   sides *automatically*, diff against an allowlist of known gaps. This is the
   "all features and modes" coverage answer, and it self-updates when upstream
   changes.
3. **Scenario parity** — one scenario file, two thin GUI drivers, one normalized
   state snapshot. Reserved for what genuinely needs a GUI.

The noVNC container (`qt/`) stays for humans. It leaves the automation path.

## The finding that shapes everything

`web/bridge.py` is a **reimplementation** of snuffler's processing pipeline, not
a call into it. `_processed_trace` (`web/bridge.py:793`), `_filter_seg` (`:749`),
`_decimate` (`:365`), `_remove_response` (`:516`), `_decon_freq_limits` (`:494`),
`_sort_order` (`:608`) and `_scale_context` (`:705`) each mirror logic that lives
in `pile_viewer.py`. That is the correct design — the browser cannot run Qt — but
it means the two can silently disagree on numbers, and **no screenshot
comparison would ever reveal it**.

Two facts make direct comparison cheap:

- **`bridge.py` imports in plain CPython.** Every `import js` and
  `from pyodide.http import pyfetch` is lazy and confined to network-only
  functions (`web/bridge.py:207`, `:213`, `:260`). With
  `PYTHONPATH=web/proto:web` and pyrocko installed, `import bridge` just works.
  The generated protobuf Python package is already emitted to `web/proto` by
  `buf.gen.yaml`.
- **Qt snuffler needs no display.** `QT_QPA_PLATFORM=offscreen` gives a real
  `QApplication` with full layout, signals, and widget state, with no X server,
  window manager or compositor. The filesystem and network friction we hit with
  the streaming container came from Xvfb + x11vnc + websockify, not from Qt and
  not from Docker.

So both implementations can be instantiated **side by side in the same process**
and asked the same question.

## Tier 1 — oracle parity

### The two seams

| | entry point | returns |
| --- | --- | --- |
| Qt | `PileViewerMain.prepare_cutout2(tmin, tmax, ...)` — `sources/pyrocko/src/gui/snuffler/pile_viewer.py:3590` | the `list[Trace]` the renderer is about to draw |
| web | `bridge._get_view_data(req: ViewParams)` — `web/bridge.py:1026` | `(ViewMeta, np.ndarray)` |

`prepare_cutout2` is the whole pipeline — chopper → demean → degap → pre-process
hooks → filtering (IIR or FFT) → response removal → rotation → post-process hooks
→ chop — and it **returns before any painting happens**. It is a pure function of
viewer state plus the pile. That makes it a genuine oracle, not a proxy for one.

Constructing the viewer headlessly:

```python
# QT_QPA_PLATFORM=offscreen, PYTHONPATH=web/proto:web
app = QApplication([])
pv = PileViewer(pile, use_opengl=False)   # see caveat below
view = pv.get_view()                      # -> PileViewerMain
view.set_time_range(tmin, tmax)
view.lowpass, view.highpass = 2.0, 0.5
view.menuitem_demean.setChecked(True)
traces = view.prepare_cutout2(tmin, tmax)
```

**Caveat:** `PileViewer.__init__` defaults `use_opengl` to `is_macos`
(`pile_viewer.py:4554`), which selects `GLPileViewerMain` — a `QOpenGLWidget`.
Pass `use_opengl=False` explicitly so the harness always gets the plain
`QWidget` subclass and never needs a GL context.

### What to compare

Per scenario, after setting equivalent state on both sides:

- **Sample data** — per trace, `np.allclose` on `ydata` with an explicit
  tolerance, plus relative RMS of the residual as the reported metric. Absolute
  equality is the wrong bar: the web side decimates for display
  (`_decimate`, `web/bridge.py:365`) where Qt draws min/max envelopes, so the
  comparison must either disable decimation on the web side or compare
  envelopes to envelopes. Decide this per scenario, explicitly — never by
  loosening the tolerance until it passes.
- **Trace geometry** — `tmin`, `deltat`, `nsamples`, the nslc set.
- **Track layout** — track order and grouping (`_sort_order` vs
  `view.get_nslc_ids_for_track`, `pile_viewer.py:4078`), track count.
- **Scaling** — per-track scale range, since `_scale_context` reimplements the
  scaling-mode matrix.
- **Derived quantities that already have a stated contract** — `_decon_freq_limits`
  documents itself as mirroring `PileViewer.get_freq_limits`
  (`pile_viewer.py:4487`); that pair deserves a dedicated property-style test
  across a grid of `deltat` / data length / passband.

### Why this tier is worth building first

It is deterministic, runs in CI in seconds, needs no serving infrastructure, and
targets the code most likely to be wrong. Every other tier depends on
infrastructure that can flake; this one cannot.

## Tier 2 — feature-inventory diff

The "compare all features and modes" question should be answered by
**extraction**, not by a hand-maintained checklist that rots the moment upstream
adds a menu item.

### Extracting the Qt inventory

Everything needed is reachable from the live widget tree:

- **Menus** — walk `PileViewerMenuBar` recursively (built at
  `pile_viewer.py:865`–`1175`), emitting `(path, text, checkable, checked,
  shortcut, enabled)` per action. Radio groups come out of
  `add_radiobuttongroup` (`:716`) with their full option set, which is exactly
  how the view/scaling/sorting *modes* are enumerated.
- **Snufflings and their parameters** — `view.iter_snuffling_modules()`
  (`:1494`) plus `Snuffling.get_parameters()` (`snuffling.py:669`). Because
  parameters are declared with `Param` / `Switch` / `Choice`
  (`snuffling.py:49`, `:95`, `:111`), this yields every knob of every built-in
  snuffling — name, range, default, unit — for free.
- **Keyboard shortcuts** — the `keyPressEvent` key map plus the `fkey_map` table
  (`pile_viewer.py:755`).

### Extracting the web inventory

Already built. `?a11y=debug` publishes the raw AccessKit tree as JSON on
`window.__snufflerA11y`, with `window.__snufflerA11yRev` counting republishes so
a reader can tell a stale snapshot from a live one
(`crates/accesskit-dom/src/tree_json.rs`, enabled at
`crates/snuffler-web-egui/src/a11y.rs:97`). Walk the app with each menu open and
collect role, name, state, value range and supported actions.

This is the payoff of the a11y work beyond accessibility itself: the web
frontend now has a machine-readable description of its own command surface, in
the same vocabulary (role + name + state) that the Qt introspection produces.

### The allowlist

Diff the two inventories into a generated parity matrix. Every legitimate
difference gets an entry in a checked-in allowlist keyed to the reason —
sandbox-hard, needs-a-server, Chromium-only, or soft, matching the buckets in
[`native-gaps.md`](native-gaps.md). Anything present in Qt, absent in web, and
absent from the allowlist fails the build.

Install the Qt side from `sources/pyrocko` at `PYROCKO_REF` rather than from
PyPI, so the inventory only moves when the pin is deliberately bumped. A pin bump
that adds upstream features then shows up as a precise list of new things to
implement or to justify — which is exactly the review we want at that moment.

## Tier 3 — scenario parity through both GUIs

Thin, and built last. One scenario format (semantic steps: load dataset, set
passband, toggle deconvolution, zoom, add pick, run snuffling), two drivers, one
normalized snapshot schema emitted after each step.

- **Qt driver** — offscreen `QApplication`, `QTest.keyClick` / `mouseClick` on
  `view`, or `menuitem_*.trigger()` for menu actions. Snapshot from the getters:
  `get_time_range` (`:1844`), `get_markers` (`:2034`),
  `get_nslc_ids_for_track` (`:4078`), plus the menu checked-state map from
  tier 2.
- **Web driver** — Playwright over the a11y overlay, addressing by role and
  name. `tests/a11y_browser_validate.ts` already proves this works end to end
  (`task test:a11y:browser`), including menu opening, checkbox toggling,
  keyboard activation and slider driving.

Keep here only what needs a GUI to be meaningful: keyboard shortcuts, marker
creation and editing, gathering and sorting changes, panel toggles, snuffling
dialogs. Anything that is really a question about numbers belongs in tier 1,
where it runs a thousand times faster and cannot flake.

## The corpus

One pinned set of inputs, loaded as **identical bytes** by both sides:

- the synthetic demo already used by `qt/make_demo.py` and the egui example,
- a small real MiniSEED file with a gap and an overlap (degap/maxlap paths),
- a multi-station, multi-channel set with StationXML (response removal,
  distance sorting, rotation),
- one file with a non-trivial sampling rate (decimation and Nyquist clamping).

Fix every clock: no `now()`, no wall-clock-derived time ranges, no randomness in
scenario generation. Both sides must be reproducible in isolation before they can
be compared to each other.

## What not to do

- **Do not assert on pixels.** egui and `QPainter` will never agree, by design.
  Rendering differences are not parity failures and a screenshot diff will drown
  real findings in noise. Capture PNGs from both sides as *artifacts for human
  review* — Qt via `widget.grab()` offscreen, web via Playwright — and assert on
  the data behind them.
- **Do not automate through noVNC.** Driving pixels over VNC to test something we
  can call as a function is strictly worse on every axis: slower, flakier, blind
  to numerical divergence, and hostage to the container's filesystem and network
  restrictions. `mise run qt-serve` remains the right tool for a human checking
  fidelity or exploring upstream behavior — that is what it was built for.

## Where it runs

PyQt5 is not currently installed on the dev machine. Two options, in order of
preference:

1. **Native** — `uv run --with pyqt5 --with numpy --with scipy ...` against
   `sources/pyrocko`, matching how `task test:seisbench` already pins its
   dependencies (`taskfile.yaml`).
2. **Batch container fallback** — if arm64 macOS PyQt5 wheels prove
   troublesome, a *headless batch* image: `docker run --rm -v "$PWD:/w"` running
   the harness directly. No published ports, no Xvfb, no x11vnc, no websockify,
   no long-lived session. The restrictions that made the streaming container
   awkward for an agent all came from that stack.

Tier 3's web driver additionally needs the app served, same as
`task test:a11y:browser` does today.

## Proposed layout

```
tests/parity/
  corpus/            # pinned inputs, or a fetch script if size is a problem
  scenarios/         # declarative scenario files, shared by tiers 1 and 3
  qt_oracle.py       # offscreen PileViewer -> prepare_cutout2 -> snapshot
  web_oracle.py      # bridge._get_view_data -> snapshot
  compare.py         # snapshot diff with per-field tolerances
  inventory_qt.py    # tier 2 extraction from the live widget tree
  inventory_web.ts   # tier 2 extraction from window.__snufflerA11y
  known_gaps.toml    # allowlist, keyed to native-gaps.md buckets
```

Task targets to add alongside the existing `test:*` group:

- `task test:parity` — tier 1, the default; no serving, no browser.
- `task test:parity:inventory` — tier 2; needs the app served for the web half.
- `task test:parity:scenarios` — tier 3; needs the app served.

## Open questions

- **Decimation.** Does the web side expose a way to request undecimated view
  data, or does the harness compare Qt's drawn envelope against the web
  envelope? The first is a cleaner comparison; the second tests more of the real
  path. Possibly both, at different scenario densities.
- **Snuffling parity depth.** Tier 2 gives us the parameter *surface* of every
  built-in snuffling for free. Comparing snuffling *output* (`rms`, `stalta`,
  `ampspec`, `polarization`, `cake_phase`) is a natural extension of tier 1 —
  the Qt snufflings are callable directly — but each needs its own oracle seam.
  Worth scoping separately once tier 1 exists.
- **Marker semantics.** Marker kinds, phase names, and the active-event notion
  are stateful in ways that may not have a single-snapshot representation.
  Likely needs a purpose-built comparison rather than a generic field diff.
