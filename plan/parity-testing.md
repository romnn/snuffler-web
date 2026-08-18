# Parity testing: proving the web port behaves like Qt snuffler

> Status: **built.** All three tiers live in [`tests/parity/`](../tests/parity/)
> behind `task test:parity`, `task test:parity:inventory` and
> `task test:parity:scenarios`; see [that README](../tests/parity/README.md) for
> how to run them and what each one compares. Drafted 2026-08-13 against pyrocko
> `64ae99fb` (the `PYROCKO_REF` pin in `mise.toml`), implemented 2026-08-14.
> Supersedes the assumption that comparing the two frontends requires driving the
> noVNC container.
>
> The design below survived contact with the code essentially unchanged. Two
> things turned out better than proposed: the Qt oracle does not call
> `prepare_cutout2` itself but **records a real paint**, which also yields the
> scaling matrix and track layout for free; and the tier-2 key inventory is
> extracted from Qt's *live* bindings rather than its help page, which is how it
> found that upstream's help page understates its own key ranges. What the suite
> found on its first runs is listed under [Findings](#findings).

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

> As built, the harness does not *call* `prepare_cutout2`; it paints the widget
> and records what the painter passed through it. Same seam, better coverage: the
> arguments then come from the painter rather than from the harness's idea of
> them (`trace_selector`, `degap`, `demean` are all read off the viewer at the
> call site), and the same pass yields `trace.minmax`, every `AutoScaler.make_scale`
> result and `track_to_nslc_ids` — which is the whole of "What to compare" below,
> obtained without reimplementing any of it.

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

  As built this is extracted rather than transcribed, and it is worth saying how:
  `keyPressEvent` is a chain of comparisons ending in
  `else: super().keyPressEvent(...)`, so "did snuffler bind this key" is exactly
  "did that fall-through run". Standing in for `QWidget.keyPressEvent` while
  pressing every candidate key answers it precisely — `event.isAccepted()` does
  not, since PyQt leaves the event accepted either way. Union that with the
  `QAction` shortcuts the menu walk already sees and the result is the live key
  surface, which is how the harness noticed snuffler's own help page claiming
  `0 ... 5` and `<f1> ... <f5>` where the code binds `0 ... 7` and `<f1> ... <f10>`.

  The web half is probed the same way rather than read: press each key Qt binds
  and see whether anything the accessibility layer describes changed. Keys that
  only pan the view or scroll tracks report as *inert* rather than unbound, since
  the layer does not describe either — and the diff says so instead of guessing.

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
- one file with a non-trivial sampling rate (decimation and Nyquist clamping),
- and, as built, two **checked-in real-world datasets**, because generated data
  cannot carry what real instruments do. Both live in `examples/` rather than
  under the corpus, because the app ships them as well (`File ▸ Examples`), so
  the bytes a user downloads are the bytes this suite measures:
  - `examples/eifel`, a recorded Eifel event over 354 stations in six networks at
    three sampling rates, with a yaml event catalog and per-network StationXML.
    On its first run it surfaced a dead channel pinned at the digitizer rail
    that the two sides scaled differently.
  - `examples/bonin-2015-05-30-m7.8-multiloc`, the GSN recording of a 664 km deep
    Mww 7.8: six stations carrying one channel at two and three location codes,
    which is the only shape "Grouped by Location" can be observed on. Real
    co-location brings what a generator would have to think to arrange — 20 Hz
    beside 40 Hz on one row, gains an order of magnitude apart, sensor depths
    that differ where the coordinates do not — and the last of those found a
    defect.

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

## Layout

As built — see [`tests/parity/README.md`](../tests/parity/README.md) for the full
file list. The proposed layout survived; what it gained was a runner per tier, a
shared scenario model, the `freq_limits.py` property grid, one key notation shared
by both web drivers, and two modules the runners share instead of repeating:
`gaps.py`, the one reader and enforcer of `known_gaps.toml`, and `harness.py`, the
process scaffolding every entry point imports first.

```
tests/parity/
  corpus/build.py    # deterministic corpus + a checked-in sha256 manifest
  scenarios/         # declarative scenario files, shared by tiers 1 and 3
  qt_oracle.py       # offscreen PileViewer -> real paint, recorded -> snapshot
  web_oracle.py      # bridge._get_view_data -> snapshot
  compare.py         # snapshot diff with per-field tolerances
  inventory_qt.py    # tier 2 extraction from the live widget tree
  inventory_web.ts   # tier 2 extraction from the accessibility layer
  known_gaps.toml    # allowlist, keyed to native-gaps.md buckets
  run_{oracle,inventory,scenarios}.py
```

Task targets, alongside the existing `test:*` group:

- `task test:parity` — tier 1, the default; no serving, no browser.
- `task test:parity:bridge` — the bridge's snuffling behaviour (marker scope,
  aggregation, failure contracts) against real pyrocko; also part of tier 1's run.
- `task test:parity:inventory` — tier 2; needs the app served for the web half.
- `task test:parity:scenarios` — tier 3; needs the app served.

## Answers to the open questions

- **Decimation — settled.** The web side does expose undecimated view data:
  `allow_downsampling = false` makes `_decimate` return raw samples, because
  `_get_view_data` then passes `len(yview)` as the bin target and the
  `n <= npx * 4` short-circuit fires. Tier 1 therefore compares samples to
  samples. Comparing envelope to envelope turned out to be the wrong second
  option anyway: Qt's `Allow Downsampling` is not display decimation at all but
  an actual `trace.downsample(2)` of the data, so the two menu items are
  different transformations sharing a name (`allow-downsampling-meaning`).
- **Snuffling parity depth — still open, and now cheaper.** Tier 2 extracts the
  full parameter surface of all 20 built-in snufflings, including every `Param`
  range and `Choice` option. Comparing snuffling *output* remains unbuilt; the
  scenario format and the corpus would carry it, but each snuffling needs its own
  oracle seam.
- **Marker semantics — partly settled.** A single-snapshot representation does
  work for kind, phase, polarity and per-marker selection, which is what tier 3
  compares now that the marker rows publish `aria-selected`. The active-event
  notion is still not covered: it is stateful across markers rather than a
  property of one.

## Findings

What the suite reported the first time it ran. Every defect it found *then* has
since been fixed and its `known_gaps.toml` entry deleted, which is the only way the
register lets a fix land: an entry that stops diverging fails the run as stale, so
the fix and the removal have to arrive together. Three defects are open at the time
of writing, all reproducing on every run — two from the sweep below, and one the
corpus found later, when real co-located instruments arrived.

**Defects found and fixed** — differences with no defensible reason, found by
tier 1 and 3:

- Three of the four Scaling menu bases (Robust, Mean ± 2σ, Mean ± 4σ) computed no
  data range at all in the default per-trace grouping, so every track rendered
  with `ymin = 0, ymax = 1` instead of being scaled.
- Every gap segment became its own track: a two-channel file with gaps stacked
  eight tracks where snuffler stacks two. Fixing it meant giving `ViewTrack` a
  repeated `ViewSpan`, so one row can carry one run of samples per segment.
- Turning on rotation reordered the tracks, swapping the two horizontal rows.
- Degapping rewrote the loaded pile in place, so the option was not reversible.
- Overlaps longer than five samples were trimmed only on the web side
  (`maxlap=None` against snuffler's `gap_lap_tolerance`).
- A freshly picked marker was not selected, so the entire marker-editing keyboard
  vocabulary did nothing until the user selected it by hand.
- Single-key shortcuts stopped working after a menu was used from the keyboard,
  and only a pointer click on the canvas brought them back. The guard asked
  `egui_wants_keyboard_input()` — true whenever *any* widget holds focus — where
  it meant `text_edit_focused()`.
- Marker rows published no selected state, so a screen reader could read the table
  and not tell what was selected in it. Tier 3 compared the aggregate count as a
  result; it now compares selection per marker.
- A dead channel — every sample at −2²³, the 24-bit digitizer rail, exactly what the
  real-world corpus exists to carry — rendered with a `[0, 1]` y range on the web,
  leaving the flatline outside its own track, where snuffler centres it at ±1 around
  the value. The bridge's degenerate-range guard treated an *equal* range as
  meaningless when pyrocko's own `AutoScaler` already pads it the way Qt shows it;
  the guard now steps in only for non-finite or inverted ranges.

Three more were filed as legitimate divergences on the first pass and reclassified
after review, which is worth recording as a caution about allowlists:

- **Deconvolution output unit** (displacement against velocity, a factor of iω).
  Justified as a deliberate choice on the strength of a docstring restating the
  code. Nothing in the browser prevented matching Qt, nothing downstream read the
  unit, and no unit appeared anywhere on screen — so a user got a different
  physical quantity with no way to notice. Defect.
- **Spherical against ellipsoidal distance.** Filed as `sandbox-hard`, repeating
  `bridge._dist_deg`'s comment that `distance_accurate50m` needs the
  `orthodrome_ext` C extension. It does not: the same function takes
  `implementation='python'` and runs the same Meeus formula in pure Python,
  measured bit-identical to the C path (0.00e+00 m) where the spherical form is
  out by up to 666 m. Defect, one keyword.
- **Azimuthal blocks.** Folded into the azimuth-wrap entry, where the defensible
  half covered for it. The wrap is a free choice that changes nothing about
  grouping; the block sort binned on a half-sector offset, so stations near a
  boundary landed in different blocks. Split out as its own defect.

Fixing the unit then exposed a twelfth difference the register had been hiding:
`decon-output-units` claimed the `samples` check for the decon scenarios, so
nobody could see that the web deconvolved *before* filtering while snuffler
restitutes `processed_traces` *after*. Same linear operators, different taper —
the frequency limits come from `data_len`. Worth remembering that an allowlist
entry masks every other difference in the checks it claims, so removing one is
how the next one surfaces.

**Location grouping**, found by reading both sides rather than by a failing check,
and the one case where the corpus was the thing at fault. Snuffler's two "Grouped
by Location" subsorts gather on three of the four code elements, so every location
of a channel lands on one row; the port mapped them to four-element orderings,
making them duplicates of their ungrouped twins. No dataset had a station
recording on two location codes, so nothing could see it. Fixed to match, which
meant following what a merged row implies: the location drops out of the row's
label and becomes snuffler's own `track_patterns` wildcard `*` in its identity
(which is what keeps markers matched to the row when their location has no data
in the window), colour moves to the span (which is why snuffler colours by
location in exactly the modes that group over it), and the amplitude scale moves,
because snuffler projects per (track, scaling key) and two sensors at one site do
not share a scale. The `multiloc` dataset makes the case reproducible, with its
second sensor an order of magnitude quieter so a row-wide scale would flatten it.

Real co-located instruments arrived next, in `bonin-2015-05-30-m7.8-multiloc`, and agreed
exactly — merged rows, mixed 20/40 Hz members, a shared scale, all at
`max_rel_rms = 0` — right up to the point where the row has to be *sorted*. Snuffler
puts the sort value inside the gather key (`set_gathering`), so with 3D distances on,
two sensors of one station at different depths land on different rows: 27 in Qt
against the web's 18. The port could not follow, because it stored one position per
`(net, sta)` and had nowhere to put the second sensor's depth.

Fixed by keying the station table the way snuffler keys its own — every station under
both `(net, sta, loc)` and `(net, sta)`, resolved in that order (`station_keys`) — and
by putting the sort value into the web's gather key too, which is what actually splits
the rows. Both sides now say 27. Note what that means: snuffler ungroups by depth what
the user asked it to group by location, and matching it meant reproducing that. The
change also settled the vertical leg, which had been open in both directions: the port
now computes `event.depth - station.depth` because snuffler does, elevation dropped and
all. That one is signed off in `vertical-leg-follows-qt` and watched by a canary, since
it is the rare entry that breaks when *upstream* is fixed.

**Genuine divergences**, pinned so they cannot change unnoticed: the demean extent
(the whole-trace processed-trace cache is a real constraint, though whether the
port chose it or inherited it is unrecorded), and Qt's `min_deltat_allow` dropping high-rate traces at
wide zoom (unreconciled: a fetch optimisation with a real cost against keeping
every channel on screen — a product call, recorded so it gets made).

The azimuth wrap was pinned here too, until the rule "a difference must be
justified" was applied to it: its own entry admitted the wrap was a free choice,
so the web now sorts through snuffler's exact gather key — azibazi's (-180, 180]
wrap, the subsorted nslc as the tie-break, missing coordinates last, the boundary
block sector split across ±180 — and the entry is gone. Making the eifel ring
match exactly also required taking station coordinates from the same
`get_pyrocko_stations` call snuffler uses, because real StationXML carries channel
positions that disagree with the station element's own by ~100 m.

**Command-surface gaps**, all accounted for: no Ctrl accelerators, no `s`-prefixed
chords, no Demean key, only the double-couple seismosizer source, no per-snuffling
help, and the GEOFON one-click presets folded into the catalog search. Plus two
accessibility gaps the diff surfaced on its own — 27 radio-group commands that
publish no selected state, and a colormap picker with no accessible name.

### The systematic sweep

The findings above are what the suite reported by running. A later pass went the
other way: seven parallel readings of both implementations — keys and pointer, the
command surface, the DSP pipeline, markers and events, layout and the time axis,
all thirteen built-in snufflings, and application lifecycle and I/O — each one
comparing source against source and holding the result against the register.

It found roughly seventy differences the register did not have, and three of them
were reproducible in tier 1 the same day:

- The lowpass and highpass stages let pyrocko's `demean=True` default fire where
  snuffler passes `demean=False`, so with Demean *off* the web still removes the
  trace mean — 6.7e-2 relative RMS, five orders past tolerance. The bandpass branch
  beside them is correct, which is what makes it an omission rather than a choice.
- FFT filtering and Deconvolution together run as two passes where snuffler folds
  the inverse response, the taper and the Butterworth into one transfer function —
  1.2e-2 relative.
- The deconvolution is not window-faithful: restituting a 60 s window and
  restituting 300 s and cutting 60 s out of it differ by 2.1e-1 in the window's
  interior, which `edge_trim_s` cannot reach.

Each got a scenario, so all three are now enforced rather than asserted. The rest
went into `[[manual]]`, which is where the sweep's real lesson is: the register
grew from six enforced entries and two manual ones to nine and fifty-four, and the
imbalance is not a property of the port. It is a map of what the tiers cannot see.
Three blind spots account for most of it — tier 2 diffs names and discards the
option sets, ranges, enabled state and the entire `snufflings` section its own Qt
extractor already emits; tier 3 snapshots menu checkables, four control values and
marker identity and nothing else; and no tier compares rendering by design, so a
difference in *what is drawn* lands in `[[manual]]` even when it changes
information rather than appearance.

That reframes the next round of work. The highest-value additions are no longer
more scenarios but wider extractors: a fourth `diff_section` over `snufflings`
would enforce most of a thirteen-panel parameter surface at once, comparing combo
choices would close the colormap and data-centre entries together, and carrying
the view time range into tier 3 would settle the navigation and follow-mode
entries. Each is small, and each graduates a group of entries rather than one.

Two habits the sweep confirmed are worth keeping. Every existing entry was re-read
against the code, and six were imprecise in the same direction — a plausible reason
that turned out weaker than stated, which is exactly how `decon-output-units` hid a
second difference before. And the sweep also read the harness, not just the port:
`scenario_qt.py` implements the `pick` action as the web's semantics hand-coded
into the Qt driver, `inventory_qt.py` builds a viewer with no panel parent so Qt's
panel togglers never exist to be diffed, and a `liberal_fetch` scenario knob
reaches the Qt oracle with nothing on the web side to receive it. An oracle that
agrees with the implementation it is testing is worse than no oracle.
