# Parity: proving the web port behaves like Qt snuffler

Measures the web port against upstream snuffler, its reference (see
[ADR 2](../../adr/0002-upstream-qt-is-a-reference.md)). Three tiers, in descending value
per unit of effort, and nothing here drives pixels over VNC.

| | what it compares | needs |
| --- | --- | --- |
| `task test:parity` | the numbers each side is about to draw, plus the bridge snuffling tests | nothing but Python |
| `task test:parity:bridge` | the bridge's snuffling behaviour against real pyrocko | nothing but Python |
| `task test:parity:inventory` | the whole command surface, menus and keys | the app served |
| `task test:parity:scenarios` | UI state through both GUIs, step by step | the app served |

Every tier runs pyrocko built from `sources/pyrocko` at the `PYROCKO_REF` pin, so the
Qt side only moves when someone bumps the pin on purpose — and a bump then shows up
here as a precise list of new things to implement or to justify.

## Tier 1 — oracle parity

Both implementations, in one CPython process, asked the same question.

The Qt side is a real `PileViewerMain` under `QT_QPA_PLATFORM=offscreen` that is
**actually painted**. During that paint the harness records the three values the
painter computes and immediately turns into pixels: the `prepare_cutout2` traces, the
`trace.minmax` data ranges, and every `AutoScaler.make_scale` result. Recording the
paint rather than re-deriving it is the whole point — the moment the harness computes
any of it itself, it stops being an oracle and becomes a second implementation that
can drift the same way the first one can.

The web side is `web/bridge.py`, imported directly with `PYTHONPATH=web/proto:web` and
called at `_get_view_data` — the same entry point the Pyodide worker calls. Asking it
for **undecimated** samples turns out to be simple: every scenario names one of Qt's
fixed visible lengths, and on that path the payload *is* the chopped samples, at the
trace's own rate unless `allow_downsampling` asked `Trace.downsample` for otherwise —
which answers the plan's open question about how to compare like with like. The
frontend's own Adaptive policy, which decimates instead of refusing channels, is the
one thing tier 1 never asks for; it has no Qt counterpart, and
`test_adaptive_visible_length.py` holds it to its own claims instead.

Each scenario also runs the web side twice and fingerprints the loaded pile before and
after, because a view computation must be a pure function of the data and must leave it
alone. That check is what caught degapping rewriting the pile in place.

Alongside the scenarios, `freq_limits.py` walks a 2352-point grid of
deltat × length × passband over `_decon_freq_limits` against `PileViewer.get_freq_limits`
— the one pair whose docstring claims to be a copy. It runs with the full suite, or
alone as `task test:parity -- decon-freq-limits`.

The scenarios on the shipped example datasets are the same comparison on recordings
nobody generated — every dataset behind `File ▸ Examples`, from 1061 traces over 354
real stations at deployment scale down to a single site recording one earthquake three
times over. Each has a control with every option off, and most also sort by distance
with every track's printed "N.N km" label held against the metres Qt's own
`set_origin` computed: at teleseismic range on `sumatra`, at 5 to 89 km on
`ridgecrest`, and on `tonga` where one site contributes two channel positions because
its barometer and its seismometer sit at different elevations. Real integer counts
survive the float32 bridge exactly, so their `max_rel_rms` is 0 — on those datasets
*any* nonzero residual is a finding, not noise.

Two are the exception, and it is worth knowing which: `tohoku` peaks at 46.8 million
counts and `sumatra` at 67.1 million, past the 2²⁴ where float32 stops counting
integers exactly, so both come back at ~3e-08 instead of 0. Everything else in the
corpus stays under that ceiling — `kilauea`'s caldera collapse gets closest at 10.5
million — which is why 0 is the normal answer here and these two are read against the
`rtol` like the synthetic datasets are.

### Reading a result

```
pass demo-bandpass-full   59/59 checks match   max_rel_rms=2.57e-08
pass demo-demean-zoom     23/59 checks match [demean-extent]
```

`pass` means every check either matched or diverged exactly as `known_gaps.toml` says
it should. `bug` means the same, except that one of the entries accounting for it is a
confirmed defect rather than a legitimate difference — there are none at present.
`FAIL` means a divergence nobody wrote down — or a documented one that stopped
happening, which fails just as loudly so the register cannot rot into folklore.

That last rule is what makes a fix land cleanly: repair the defect and the entry
covering it fails as stale, so the code change and the register change have to arrive
in the same commit. Every `[[defect]]` this suite found on its first runs has now gone
that way. Removing one is also how the *next* difference surfaces — an entry masks
every other disagreement in the checks it claims, and dropping the deconvolution-unit
entry immediately exposed that the web deconvolved before filtering where snuffler
restitutes after.

The line between `[[numeric]]` and `[[defect]]` is the one worth being strict about,
because it is easy to get wrong in the comfortable direction. Three entries started as
"legitimate" and moved to `[[defect]]` on review: two rested on a plausible reason that
turned out to be false when checked (the ellipsoidal distance is *not* unavailable in
Pyodide — `distance_accurate50m(..., implementation='python')` is bit-identical to the C
path), and one hid an unjustified difference inside a defensible one. An entry earns
`[[numeric]]` by naming a constraint someone can verify, not by sounding reasonable.

`max_rel_rms` is the worst relative RMS residual over all traces in the scenario. On a
clean scenario it sits at ~2.6e-8, which is the float32 the payload crosses the bridge
in and nothing else.

## Tier 2 — feature-inventory diff

Extraction on both sides, never a checklist.

`inventory_qt.py` reads Qt's surface off the live widget tree: the menu bar walked
recursively, every built-in snuffling with the `Param`/`Switch`/`Choice` knobs it
declares, the control strip, the shipped help tables — and the **live key bindings**,
asked rather than read. Snuffler's key map is a chain of comparisons inside
`keyPressEvent` ending in a fall-through to `QWidget`; standing in for that
fall-through during a probe answers "is this key bound" exactly, which is how the
harness noticed that snuffler's own help page understates its marker-kind and phase-key
ranges.

`inventory_web.ts` reads the web surface off the accessibility layer — the DOM
projection for role, name, state and value range, and `?a11y=debug`'s AccessKit tree
for supported actions. It walks every menu, captures the control strip in both view
modes, reads the app's own help window, and **presses every key Qt binds** to see
whether anything observable happens.

`run_inventory.py` diffs them. The rule is one-directional: anything Qt offers that the
web does not and `known_gaps.toml` does not excuse fails the build. Web-only entries
are reported and never fail — the port is allowed to be more than a copy.

Wording differences between the two frontends live in the `[[alias]]` table, decided
once by a human, so the diff stays exact everywhere else.

## Tier 3 — scenario parity through both GUIs

Thin, and built last. One scenario format, two drivers, one snapshot schema per step:
menu checkbox states, control-strip values, and the marker table.

The Qt driver drives a real offscreen viewer inside a real `QMainWindow` — menu actions
triggered, key events delivered, picks placed through the same `start_picking` /
`stop_picking` the mouse handlers call. The web driver drives the served app through
the accessibility overlay with `?a11y=pointer`, exactly as
`tests/a11y_browser_validate.ts` does.

Two things are deliberately **not** compared:

- **View time range and track order.** The web frontend paints them and does not
  publish them to its accessibility layer, so there is nothing on that side to read.
  Extending the a11y layer is the natural way to widen tier 3.
- **Where a pick landed.** A fractional canvas position becomes a different time and a
  different track under the two layouts. What the marker *is* — kind, phase, polarity,
  and whether it is selected — is compared; where it went is recorded for a human.

The driver deliberately does *not* click the canvas before sending a keystroke. It used
to, to work around single-key shortcuts dying after a menu was operated from the
accessibility layer; with that fixed, clicking first would only hide a regression of
exactly the kind a keyboard-only user would hit.

## The corpus

The synthetic datasets are generated, not checked in: `demo.mseed` alone is 4 MB.
What *is* checked in is `corpus/manifest.json`, the sha256 of every file, verified on
every run — the corpus is the fixed input each number is measured against, so silent
drift is never ignorable.

| dataset | what it is for |
| --- | --- |
| `demo` | the synthetic demo `bridge._make_example_scene` produces, byte for byte |
| `gaplap` | gaps and overlaps at every size the degap logic branches on, aligned and off-grid |
| `multi` | 4 stations × 3 components with coordinates, an event and poles-zeros responses |
| `multiloc` | co-located sensors: two location codes at a site, the second an order of magnitude quieter |
| `oddrate` | 5 Hz, 7 Hz and 50 Hz in one pile: Nyquist clamping and mixed-rate selection |
| `eifel` | real recordings: 354 stations, six networks, three rates, a yaml event catalog, per-network StationXML |
| `bonin-2015-05-30-m7.8-multiloc` | real recordings: six GSN stations carrying one channel at two and three location codes |
| `tohoku-2011-03-11-m9.1-clipping` | real recordings: one site, three instruments, three rates, one of them off scale |
| `sumatra-2004-12-26-m9.1-surface-waves` | real recordings: the corpus's slowest, LH channels at 1 Hz over six hours |
| `ridgecrest-2019-07-06-m7.1-local` | real recordings: the only local one, seven stations 5 to 89 km out at 100 Hz |
| `tonga-2022-01-15-air-wave` | real recordings: barometers beside seismometers, over a 22.5-hour window |
| `kilauea-2018-07-15-caldera-collapse` | real recordings: 100 Hz volcanic seismicity spanning four orders of magnitude in one trace |

Only `demo` uses an RNG, and only because it has to be that dataset; everything else is
analytic and stable under any numpy. The task targets pin numpy for the same reason.

Everything from `eifel` down is checked in rather than generated (the bytes as the
datacenter delivered them), because their value is exactly that no generator made them.
They live in [`examples/`](../../examples/) rather than here, because the app ships
them too — `File ▸ Examples` fetches each as one bundle — so the bytes a student
downloads are the bytes this suite holds both frontends to. Every dataset the app
offers is measured here, which is the only way that sentence stays true. Each records
the queries it came from in its own `example.toml`: provenance, not a generator, since
StationXML is stamped with its creation time and a re-fetch drifts the manifest
deliberately. The manifest pins these files too, so a locally edited copy fails the
same way.

Three of them need a `visible_length` in their scenarios. Qt fetches nothing finer than
`min_deltat_allow`, derived from the window and that budget, so at the default a
22.5-hour window or an hour of 100 Hz leaves it drawing empty rows — the divergence
`oddrate-coarse-window` exists to document. Raising the budget until Qt keeps every
trace turns those scenarios back into comparisons of recordings rather than of a fetch
policy. It is a Qt-side knob; the web decimates whatever it is given.

`eifel` is a deployment: on its first run it found what only one carries, a dead channel
pinned at −2²³, which the web port scaled to a `[0, 1]` range that left the flatline
outside its own track where snuffler centres it. `bonin-2015-05-30-m7.8-multiloc` is the opposite
shape — six stations, chosen because each records the same ground motion two or three
times over. Co-located sensors are the only data on which "Grouped by Location" means
anything, and real ones bring what the synthetic `multiloc` cannot arrange: 20 Hz beside
40 Hz on one row, gains an order of magnitude apart, a −515 000-count offset beside a
partner at −628, and sensor depths that differ where the coordinates do not. That last
one is what found the station table's missing per-location position — invisible under
every sort but the 3D distance one, where the depth reaches the gather key and pulls a
merged row apart.

To rebuild after changing a generator: `python tests/parity/corpus/build.py --force
--update-manifest`, and review the parity deltas in the same commit.

## known_gaps.toml

The single register of every intentional difference — if the two sides behave the
same, nothing is written down; if they differ, the difference has an entry here or
the suite fails. One file, five kinds of entry, one reader (`gaps.py`):

- `[[numeric]]` — a place where the two legitimately compute different numbers.
- `[[defect]]` — a difference that is a bug, found by this suite and not yet fixed. Not
  an excuse: a register, with the fix written into the entry. Scenarios naming one
  print as `bug` and the run lists every one that still reproduces.
- `[[feature]]` — a command Qt has that the web does not.
- `[[manual]]` — a difference no tier can observe yet, recorded so it is not silent.
  `enforce` names the check that would observe it, and is required: the kind's whole
  promise is that an unobservable difference still has a way out of that state.
- `[[alias]]` — the map between the two frontends' wordings.

The line between `[[manual]]` and the enforced kinds is mechanical rather than
editorial. `evaluate` only ever looks at entries a scenario named, so a `[[numeric]]`
or `[[defect]]` entry nothing names is inert — it excuses nothing and can never go
stale, which is indistinguishable from one that has quietly rotted. A full tier-1 run
fails on any such entry. That rule is what moved `waterfall-pipeline` into
`[[manual]]`, where it always belonged.

Every entry is either **signed off** — its `why` justifies the difference, and a
difference really must be justified; "free choice" is a reason to match, which is
how the azimuth wrap stopped existing — or marked **`todo = true`**: open work the
diff excuses without mistaking it for a decision. Tier 2 prints the two apart, as
`gap` and `todo`.

Every entry has to keep applying. A numeric or defect entry that stops diverging, and a
feature entry that stops matching anything, both fail the run as stale.

An entry covering `samples` can also say what *shape* it excuses: `residual =
"constant"` makes the run verify that the divergence vanishes once each segment's
constant offset is removed. That re-arms the samples check under the excuse — the
demean-extent entry can no longer hide a second, unrelated sample defect in the
scenarios it covers, which is precisely how the decon-taper difference stayed
hidden once.

An entry can likewise say *where* it excuses: `subjects.<kind>` lists glob
patterns over the finding subject, so the min-deltat entry excuses geometry and
scales only on the channel Qt actually drops, and the 3D-distance entry only on
the stations with elevation or depth — a divergence anywhere else in those kinds
fails as unexplained, and a strict entry has to keep reproducing inside its own
scope.

### What the register does not yet reach

Most of `[[manual]]` traces back to three blind spots, and closing any one of them
would graduate a whole group of entries at once:

- **Tier 2 diffs names.** It never compares a combo box's option set, a control's
  range, a menu item's enabled or checked state — or the `snufflings` section, which
  its own Qt extractor already emits in full, with every `Param`, `Switch` and `Choice`
  and their defaults, bounds and choices. Nothing consumes it. That single missing
  `diff_section` is what leaves the whole snuffling parameter surface unenforced.
- **Tier 3 snapshots a narrow slice.** Menu checkables, four control values, marker
  identity. Not the view time range, the track order, marker placement, or any state
  belonging to a window it did not open — which is why no waterfall control is compared
  by anything.
- **No tier compares rendering, by design.** Anything whose only symptom is what gets
  drawn lands in `[[manual]]` even when it changes information rather than appearance.

Two more are narrower but sharper: `compare_annotations` returns early unless the sort
is `distance`, so the azimuth, backazimuth and block scenarios compare no annotation at
all; and `compare_tracks` filters Qt's empty tracks away, which is a decision about
layout parity that now lives in `known_gaps.toml` as `track-stack-membership` rather
than only in a comparator docstring.

The enforcement is itself tested: `test_gaps.py` pins the edge cases of
`gaps.evaluate` and the register validation — scoping, per-kind staleness, the
residual shape check, malformed entries — because a bug there would not show as a
failing scenario but as one passing for the wrong reason. `task test:parity` runs
it before the scenarios; `task test:parity:unit` runs it alone.

## Running it

```sh
task test:parity                       # tier 1, a few minutes, no browser
task test:parity -- -v                 # per-check detail
task test:parity -- demo-bandpass-zoom # one scenario
task test:parity -- decon-freq-limits  # just the property grid
task test:parity:bridge                # the bridge's snuffling tests alone

task serve &                           # tiers 2 and 3 need the app served
task test:parity:inventory
task test:parity:scenarios
```

Artifacts land in `.artifacts/`: `tier1.json`, `tier2.json`, `tier3.json`, both
inventories, and screenshots grouped by tier and scenario.
Tier 2 and Tier 3 write `qt-full.png`, `qt-waveforms.png`, `web-full.png`, and
`web-waveforms.png` for each captured final state.
The waveform files come from each application's own PNG export path.
Tier 1 writes both Qt views plus `web-waveforms.png`; it has no `web-full.png`
because that tier deliberately imports the Python bridge without running a browser.

The PNGs are for human review only.
Nothing asserts on pixels, because egui and `QPainter` will never agree and a
screenshot diff would drown real findings in noise.

Reuse a previous run's expensive half while iterating:

```sh
python tests/parity/run_inventory.py --no-web    # reuse the last web inventory
python tests/parity/run_scenarios.py --no-qt     # reuse the last Qt snapshots
```

## Layout

```
corpus/build.py     deterministic corpus + sha256 manifest
(the checked-in real-world datasets live in ../../examples/, shared with the app)
scenarios/*.toml    declarative scenarios, shared by tiers 1 and 3
scenario.py         the scenario and dataset model both tiers read
harness.py          process scaffolding: environment, import paths, colours
gaps.py             known_gaps.toml loaded, validated and enforced
test_gaps.py        pytest edge cases for that enforcement
qt_oracle.py        offscreen PileViewer -> real paint -> snapshot
web_oracle.py       bridge._get_view_data -> snapshot
compare.py          snapshot diff with per-field tolerances
freq_limits.py      property grid for the one documented copy
run_oracle.py       tier 1
../../crates/snuffler-web-render/examples/parity_artifacts.rs
                    browserless Tier 1 web waveform renderer
inventory_qt.py     tier 2, Qt side
inventory_web.ts    tier 2, web side
run_inventory.py    tier 2
scenario_qt.py      tier 3, Qt driver
scenario_web.ts     tier 3, web driver
parity_browser.ts   browser launch and renderer policy shared by tiers 2 and 3
run_scenarios.py    tier 3
parity_keys.ts      one key notation, shared by both web drivers
known_gaps.toml     every accounted-for difference
```

## Theme screenshots

The standalone theme sweep uses the same browser launcher, readiness, menu handling and
full-page screenshot helper as these tiers:

```bash
bun scripts/screenshot-themes.ts --help
bun scripts/screenshot-themes.ts --examples tohoku-2011-03-11-m9.1-clipping --grouping grouped
```

Without filters it captures every served example × theme × supported light/dark mode ×
boxes on/off × coloured traces on/off × grouped/separate locations × fitted/zoomed view.
Results are gitignored under `tests/parity/.artifacts/screenshots/themes/<theme-name>/`;
each PNG has a flat name containing the other dimensions. A root `captures.jsonl` records
the verified settings and URLs. Snuffler Qt contributes only its supported light canvas.
