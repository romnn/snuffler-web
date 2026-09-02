# Rendering-determinism review (deep multi-agent)

Triggered by a live bug the ensemble process itself initially misdiagnosed: a wiggle's
colour flipped blue→orange on a small pan (IU TUC BH2, the multiloc example). The palette
theories were wrong; probing the running app showed the reply's *span order inside one
row* was window-dependent (chopped-tmin ordering across mixed sample rates), and the
last-drawn span wins the overlapping pixels. That fix (`01eca90`) defined the class this
review then swept for: **anything drawn that depends on the window, the track page, the
request cadence, or Python iteration order instead of the data.**

## Protocol

- Reviewers: GPT-5.6 Sol (xhigh, codex, read-only) with the Python view-production lens;
  Opus 5 (read-only) with the numerical/geometry lens. One shared self-contained brief,
  seeded with the two verified bug mechanisms; pinned pyrocko source cited by both.
- Orchestrator verification: every implemented finding re-checked against source; the two
  reports' one factual conflict (chopper batching) settled against `pile.py:1313-1322`
  (`tinc=None` → exactly one yield); live-browser probes for what static review cannot
  exercise (colour stability under the original repro gesture, waterfall render, reply
  coherence).

## Confirmed and fixed (with the finder)

| Finding | Fix |
| --- | --- |
| Waterfall dropped span-less rows from `meta.tracks`, so the reply stopped being the row universe: the scrollbar died (`max_off` hit 0), and each shorter reply shrank the next request — a ratchet down to the leading occupied rows (both; Opus supplied the feedback-loop mechanism and the minimal fix) | The row stays in both modes; the painter already draws a strip-less row as an empty labelled row. Parity test pins the universe |
| Waterfall decimation had no absolute anchor and its stride came from the clipped slice length: a one-sample pan re-sampled every column (shimmer instead of sliding), and the stride could jump on a pure pan (both) | Stride derives from the window; the take is phase-anchored to the trace's own sample grid, exactly as `_display_envelope` anchors its bins (and as Qt anchors on `tr.tmin`) |
| Adaptive envelope stride derived from the clipped slice length — a sub-sample pan crossing an integer boundary re-binned the whole trace a quarter-octave rung away (both) | `_display_envelope` takes the window length and derives the stride from it; parity test pins stride stability under sub-sample pans |
| Waterfall Abs colour range was page-global while the state contract promises per-track: a scroll recoloured tracks that stayed visible (codex) | The range combines per row, over that row's segments only |
| The polyline dropped samples outside a ±4 px x-band, so at picking zooms the wiggle detached from the canvas edges and its ends snapped under a pan (both) | Points are emitted and the existing clip rect cuts the crossing segment (Qt's approach). The regression test then caught a second half neither reviewer saw: the slice's *end bound* itself (`ceil`, end-exclusive) excluded the sample past the right edge — now `floor + 2` |
| `npx` was budgeted on `n_draw`, which shrinks on the last partial page: scrolling to the stack's bottom changed the horizontal resolution and forced a re-fetch (and with ADS+lowpass, a full cutout reprocess) from a purely vertical gesture (Opus) | `TrackGeom` gains `n_page` (the pre-clamp full page); the cached count and the payload budget use it |
| Fresh, correctly-fenced view replies were discarded whenever a newer `desired` existed — during a continuous drag every mid-gesture reply lost to the always-newer desired params, freezing the canvas at the pre-gesture window (Opus) | Every reply passing the sequence/generation fences is applied; the newer request still dispatches (`desired` vs `applied` comparison unchanged) |
| The painter coloured the previous reply's tracks with the *live* subsort during a subsort-change round trip — a transient palette belonging to neither state (Opus) | `ViewMeta` carries the subsort it was produced under; the painter colours from the reply |
| A blank location code could enter/leave the palette numbering with the window under a grouped subsort in piles mixing blank and real locations — the same class as the fixed numbering bug, and the residual knowingly left by that fix (Opus; anticipated in the simplification pass) | The label fallback is active only when the pile has NO usable coloring key (the single-location case it was written for); in mixed piles the blank location keeps its own pile-wide-stable `""` key |
| Same-nslc ties in the member ordering still fell through to chopper order (both, Suspected) | `deltat` joins the ordering key between nslc and tmin |
| The FFT transfer-function cache ignored StationXML response epochs: two windows straddling an epoch shared whichever TF was computed first (codex) | The response resolves before the lookup and a content fingerprint rides the key (object identity would defeat the cache — each resolve builds a fresh object) |
| Non-FFT response removal sat inside the chopper-batch loop mapping over the accumulated list — earlier batches would be restituted once per later batch (codex). Adjudicated **latent**: `tinc=None` makes the chopper yield exactly once today (`pile.py:1313`), so no live corruption; the placement was a trap | The pass moved outside the loop; one application per trace whatever the batching |

## Adjudicated as not-defects (recorded, unchanged)

- **Cutout containment reuse** (processed traces reused when the new window is contained):
  documented Qt parity (`pile_viewer.py:3639`), history-dependence and all. Both reviewers
  agree it is Qt's own behaviour, reproduced deliberately.
- **The 2000-marker overload cliff**: the threshold's position is Qt's own
  (`with_key_in_limited`); silencing the per-trace snippet layer above it is a registered,
  deliberate divergence (`marker-overload-full-quiet`) with a stated per-frame cost
  argument. A softer degradation is a design change, not a bug fix.
- **Group scaling scoped to shown tracks** (changes with the scroll): matches Qt's
  `shown_tracks_range` scoping line for line.
- **Pick scope from in-window spans**: window-dependent, and so is Qt's
  (`track_to_nslc_ids` rebuilt per repaint). Parity.

## Residuals (documented, not fixed here)

- Waterfall **Integrate** still cumsums from the slice edge, so the integral's level moves
  with the window origin; a stable integral origin is a design question (what does an
  absolute origin mean for a windowed display?), not a mechanical fix.
- A page-sparse old snapshot is still painted against the new scroll/window while its
  replacement runs (blank newly-exposed rows for one round trip). The mid-drag reply fix
  shortens the transient; replacing it entirely needs a loading-state design (codex's
  finding 9).
- `_stable_bin_stride`'s double ceiling makes every exact power of two land one rung high
  (documented in its docstring now); feeding the ladder the raw float with an epsilon
  would refine it but changes every adaptive envelope, so it is left for its own change.
- Scale annotations dedup on scale *values* rather than scaling-key identity, so the
  number column can gain/lose an entry as two sensors' autoscaled brackets coincide
  (Opus, Suspected, layout-only). The fix wants a scaling-key id on `ViewSpan`.
- Live browser exercise of the waterfall scrolling fix needs a dataset with more rows
  than fit plus data gaps; the parity test pins the universe contract instead.

## Attacked and held (do not re-open)

Both reviewers independently confirmed, with cites: the reply pipeline cannot interleave
(one in-flight view job; `ViewData` swaps as one `Arc`; the colour map is built from the
same reply it paints), `t0_rel` arithmetic is exact (Sterbenz subtraction, reply-anchored
reconstruction), the envelope's *phase* anchor holds (only the stride input was unstable),
the row universe and every colour/box/annotation input reach no unordered iteration
(pile chops are `full_id`-sorted before the bridge sees them), `station_band_colors` is
window- and page-independent in wiggle, marker↔row matching is identity-based, and the
two earlier fixes (member ordering, empty-grouped-row numbering) hold — the residuals
they left are exactly F7/F9 above, now fixed.

## Validation

`task check`, `task lint:fc` (35 combos), `task test` 133/133 (new: the boundary-crossing
polyline regression), `task test:parity:bridge` 46/46 (new: waterfall row universe,
adaptive stride pan-invariance; plus the earlier span-stack test), `task typos` — all
green. Live re-verification against the running dev build: the original repro gesture
(pan/zoom over IU TUC BH2, web theme) stays one colour across every window with all of
this in place.
