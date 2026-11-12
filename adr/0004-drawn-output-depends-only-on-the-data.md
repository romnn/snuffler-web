# 4. Drawn output depends only on the data

**Status:** accepted · **Date:** 2026-09-01

## Context

A wiggle changed colour from blue to orange on a small pan (IU TUC BH2 in the multiloc
example).
The palette was not at fault: the order of spans inside one row depended on the window,
because chopping across mixed sample rates sorted by the chopped `tmin`, and the last span
drawn won the overlapping pixels.
A sweep for the same class of bug then found a dozen more, among them waterfall columns that
re-sampled on a one-sample pan, an envelope stride that jumped between rungs, colour ranges
computed per page instead of per track, and replies discarded mid-drag.

## Decision

**What is drawn is a function of the data and the user's settings, not of the viewing
history.**
Nothing visible may change because of the time window's exact origin, the track page, the
request cadence, or Python's iteration order.
In practice:

- every ordering that reaches the screen has a total key that ends in data identity
  (nslc, then `deltat`, then `tmin`), never in chopper order;
- decimation, envelope strides and waterfall columns are anchored to absolute sample
  positions, so a pan slides the picture instead of re-binning it;
- colour numbering, colour ranges and scales are computed per track or per row from that
  row's own data, never from whatever page or window happens to be loaded;
- the view reply is the whole row universe, including rows with no span in the window.

**Integrated waterfall has a fixed origin.** The inclusive cumulative sum starts at the first
sample of each continuous source segment, with gaps resetting the sum. Filtering and demeaning
use that whole segment; integration precedes display decimation and cropping. Only the requested
rows (and any rotation partners) are processed. This deliberately differs from Qt's crop-relative
integral, whose level changes while panning.

**View refreshes use the activity indicator.** While a scroll or pan waits for a new
payload, the canvas keeps its latest snapshot. The shared activity readout shows
`Updating view` only after the refresh has been pending for 500 ms, keeping ordinary
interaction free of transient loading badges.

**Upstream's own window-dependence is reproduced, not fixed.**
Where Qt snuffler itself depends on the window, the web app follows it, because parity with
upstream outranks this rule (see ADR 2):

- processed traces are reused when the new window is contained in the old one;
- group scaling is scoped to the shown tracks;
- pick scope comes from the spans in the window;
- the per-trace marker layer goes quiet above Qt's own marker limit (registered as
  `marker-overload-full-quiet`).

## Consequences

- A new rendering input has to say what it is anchored to; "the current window" is only an
  acceptable answer where upstream says the same.
- Some state that was free to compute per reply now has to be carried per row, such as the
  subsort a reply was painted with. Scale annotations use the scaling group's identity, so
  equal autoscaled numbers do not merge two sensors' annotations.
- These were checked and hold, so they need no re-examination: the reply pipeline cannot
  interleave (one view job in flight, `ViewData` swapped as one `Arc`, colours built from the
  reply they paint), `relative_start_s` arithmetic is exact, and pile chops are sorted by
  `full_id` before the bridge sees them.

## References

- `web/snuffler_bridge/view.py` (`get_view_data`, `_display_envelope`, `_stable_bin_stride`)
- `crates/snuffler-web-render/src/paint/`
- `tests/parity/test_view_span_order.py`, `tests/parity/known_gaps.toml`
