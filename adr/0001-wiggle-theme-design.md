# 1. Wiggle theme design

**Status:** accepted · **Date:** 2026-09-05 · **Supersedes:** the hand-picked palettes before `f1106b6`

## Context

snuffler-web ships one fidelity theme (**Snuffler Qt**) and six of its own. Colour here is
not decoration: a box colour and a stroke colour are *statements about the data*, so two
classes drawn in one colour is a defect, not a blemish. Several rounds of hand-picked
palettes failed on exactly that, and the failures were only ever caught by eye.

## The goal that outranks the rest

**The themes must look good and read well.** Everything below is a floor, not a target. A
palette that clears every number and looks like a rainbow test card has failed. When a
constraint and the look genuinely conflict, say so and get a decision — do not silently
trade the look away (that has happened, and it was wrong both times).

## Invariants — never break these

1. **Meaning is snuffler's, only the colours differ.** A box is the pile's
   `determine_box_styles` index mod 7; a coloured stroke is its coloring key (location for
   `nslc`/grouped, channel for `nscl`/`sncl`/`lnsc`, `net.sta.loc` for `cnsl`) numbered over
   the *sorted pile-wide* key list; a marker is its kind 0–7. Never re-key any of them to
   make a picture look better.
2. **Snuffler Qt is pinned to upstream's constants** (`graph_colors`, Tango box styles at
   `box_alpha` 100, `aluminium5` ink, `g_color_b` markers) and is **light-only** — its
   palette is built for white and is not snuffler if moved.
3. **Every web theme has both canvases**, fitted independently. Only the *hue per index* is
   shared, so a class keeps its identity across a theme switch (drift < 15°).
4. **The empty canvas is never tinted.** Plain white or plain near-black.
5. **Web themes draw no box outlines.** The fill is the box. Do not stack outlines *and*
   block colours *and* trace colours — pick one carrier per distinction.
6. **Marker kind N keeps its Tango family hue** (< 12°); kind 7 stays grey (chroma < 0.03).
   "Kind 2 is blue" is a fact users carry between the two apps.

## Enforced floors

All distances are OKLab ΔE × 100, where ~2 is the smallest difference the eye resolves at
all. Enforced in `crates/snuffler-web-render/src/themes/mod.rs`.

| Quantity | Floor | Currently |
| --- | --- | --- |
| Ink vs canvas | ≥ 7.0 | 7.2 – 15.4 |
| Block tint vs canvas | 1.08 – 2.6 | 1.18 – 2.50 |
| Ink over any block | ≥ 3.0 | 3.2 – 6.8 |
| Stroke vs canvas | ≥ 4.5 | 6.4 – 8.3 |
| Stroke over any block | ≥ 3.0 (3.5 if always coloured) | 3.1 – 3.7 |
| Worst of the 21 block pairs | ≥ 6.0 | 6.6 – 12.0 |
| Worst of the 21 stroke pairs | ≥ 10.0 | 10.5 – 13.5 |
| **First two coloring keys** | **≥ 22** | 22.9 – 42.4 |
| **First three coloring keys** | **≥ 18** | 18.8 – 27.0 |
| Worst of the 28 marker pairs, idle and lit | ≥ 11 | 14.3 – 16.2 |
| Marker idle vs canvas | ≥ 3.0 light, 4.3 dark | 3.1 – 4.3 |
| Marker lit vs canvas | ≥ 2.2 light, 4.5 dark | 2.3 – 5.9 |
| Marker lit brighter than its own idle | strict | 1.22 – 1.57× |
| Chrome: zero line / separator / grid / axis text | 1.3 / 1.2 / 1.1 / 4.5 | held |
| Stroke chroma | ≥ 0.06 | 0.059 – 0.077 |

## Optimization goals, in priority order

Each saturates — past the cap, spend the freedom on the next goal down.

1. **Cohesion.** Seven hues and *their chroma relationships* come from a palette designed
   as a family. Chroma is what makes a muted blue read as part of an earthy set instead of
   a cyan, so it stays near what it was seeded at and separation comes from lightness.
2. **Front-loaded separation.** Real data has two or three coloring keys far more often
   than seven. Snuffler's own ramp is built that way — red, blue, green: 30.9 apart over
   its first three, falling to 9.2 over all seven. Order the ramp furthest-first; do not
   spread seven colours evenly.
3. **Worst pair over all 21**, never adjacent pairs only. Neighbour-only checking is what
   let index 1 and index 5 land on the same colour.
4. **Colour-vision deficiency**, soft: maximise the worst pair under Machado protan/deutan/
   tritan once the above are satisfied. Currently 4.3 – 6.2 on blocks.

Light and dark are fitted **jointly**, scored on whichever canvas separates worse, because
a key keeps one index across a theme switch and so the two canvases share one ordering.

## Defaults

| Setting | Snuffler Qt | Web themes |
| --- | --- | --- |
| Canvas | light, pinned | follows the app |
| Show boxes | on | **on light, off dark** |
| Color traces | off (upstream boots so) | house: on · moods: only when the boxes cannot separate |
| Track separators | off | on |

"Only when the boxes cannot separate" means the boxes are hidden **or** the subsort groups
locations. All three are `Option<bool>`: the user's click outranks the default and is what
gets persisted — an untouched setting is stored as *absent*, so it keeps following the
canvas instead of freezing at whichever one was showing when settings were first saved.

With the boxes hidden, a web theme's coloured strokes key on the **box style** so the class
the boxes carried moves onto the wiggle — **except on a location-grouped row**, which keeps
keying on location. The box style steps per contiguous run, so every trace of such a row
starts in the same run and would collapse to one colour.

## Couplings that are easy to rediscover the hard way

- **Ink weight ↔ block weight.** A grey ink at snuffler's 7.3:1 cannot read over a heavy
  block: at Qt's own 3.98 comfort level it caps blocks near 1.87, which is why snuffler's
  boxes are pastel. We keep blocks bold and hold the 3:1 graphical-object floor instead.
- **On white, "selected" is lighter and therefore *lower* contrast.** A lit marker may sit
  below its idle shade against the canvas; selection reads as a lift against its own idle
  shade, as snuffler's does (butter2 reaches only 1.5 on paper).
- **A chroma floor costs separation but is not optional.** Without it the fit parks an
  entry at the bottom of the lightness band, where chroma collapses and a "coloured" trace
  comes out black. This is why the ramp floors sit below snuffler's.
- **On a dark canvas "brighter" runs into white**, where every marker kind becomes the same
  pale tint. Lit shades lift less there and keep their chroma.

## Consequences

Adding a theme is one `WiggleTheme` variant plus one module under
`crates/snuffler-web-render/src/themes/`; the generic tests sweep it the moment it is
dispatched. Divergences from snuffler are registered in `tests/parity/known_gaps.toml`
(`color-traces-theming`, `marker-palette-theming`). The palettes are fitted numerically, so
they are regenerable rather than hand-maintained — but the *look* is still judged by eye,
on screenshots, in both canvases.
