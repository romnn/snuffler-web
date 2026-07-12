# Dual-frontend architecture: workspace layout, core extraction, and a future leptos UI

Architecture review of the Rust side of snuffler-web, focused on two requirements:

1. Keep the door open for a second, HTML-DOM frontend (leptos + tailwind, still wasm)
   next to the egui one, sharing as much code as possible with minimal boilerplate.
2. Adopt a proper cargo workspace: virtual root manifest, all crates under `crates/`,
   no top-level `src/` tree. All crates carry the `snuffler-web-` prefix (this project
   is snuffler-web; the existing `snuffler-proto`/`snuffler-*-proxy` crates get renamed
   in the same reorg).

The only hard external constraint is that the GitHub Pages deployment keeps working;
everything else (file locations, Trunk config, script paths, crate/bin names) is free to
move — no backward-compatibility requirements.

## Verdict: sound shape, needs packaging + one seam fix

The dominant architectural decision in this project — all computation lives in pyrocko
behind a protobuf contract, Rust only renders — is exactly right, and it is the reason a
second frontend is cheap at all. The bridge contract (`proto/`), the Pyodide worker, the
proxies, and the decoded-prost-messages-as-domain-model decision are all frontend-agnostic
already. This is not a local maximum; nothing fights the domain.

What the dual-frontend requirement actually demands is small:

- **One accidental coupling**: `Bridge` takes `&egui::Context` in ~35 methods, used for
  nothing but `request_repaint()` (plus one clock read). That is a repaint *waker*, not an
  egui dependency — abstract it and the entire data/transport layer is toolkit-free.
- **Packaging**: the egui-free layer (state, markers, time axis, seedlink framing,
  snuffling configs) and the backend-abstract waveform renderer exist today but are
  interleaved under egui-owned files and enforced only by convention. Splitting crates
  makes "the data layer must not know egui" a build error (see the enforcement section).
- **Nothing else.** No UI abstraction layer, no message bus, no adapter traits. The widget
  tree (menus, dialogs, forms, marker table) is per-toolkit by design — that surface is
  precisely *why* one would want a DOM frontend, so sharing it would defeat the purpose.

So: yes, a clean leptos frontend is possible, and preparing for it does **not** mean
speculative scaffolding. Every change below pays rent for the current egui app on its own
(testability, smaller files, killing the 1100-line `fn ui`, the workspace layout).
The leptos crate itself should not be created until someone actually builds it — but the
workspace is laid out *as if it already existed*, so adding it later touches nothing
outside its own directory.

## What this code is for (domain, no code vocabulary)

A seismic waveform inspector in the browser: it displays stacked seismogram traces (or
their spectrogram/waterfall form) over an adaptive time axis; a seismologist pans and
zooms through recorded or live-streamed ground motion, filters and scales it, picks phase
arrivals as markers, associates them with catalog earthquakes, and runs analysis tools
(spectra, STA/LTA detection, theoretical arrivals, synthetics, ML pickers). All signal
processing happens in real pyrocko; the Rust side owns only what the user sees and
touches.

## How the UI ↔ data-layer separation is enforced (not just encouraged)

The goal is that the layers *cannot* leak into each other, not that they shouldn't.
After the split, three mechanisms stack up:

1. **Manifest-level (compiler-enforced, the main one).** `snuffler-web-core` and
   `snuffler-web-render` simply do not list `egui`/`eframe`/`epaint` in their
   `Cargo.toml`. Any `use egui::…` inside them is then a hard compile error — there is
   no way to reference a crate that isn't a declared dependency. This is why the split
   must be *crates*, not modules: module-level convention is exactly what allows today's
   silent leak.
2. **Direction-level (cargo-enforced).** Cargo forbids dependency cycles, so once
   `snuffler-web-egui` depends on `snuffler-web-core`, core can never grow a dependency
   on the frontend — neither its types nor its functions can flow "down". The reverse
   direction (frontends consuming core) is the intended one.
3. **Manifest-drift guard (CI-enforced).** The one residual hole is someone *adding*
   `egui` to core's manifest. Close it with cargo-deny's per-crate bans, which express
   exactly "egui may only appear underneath the frontend crates":

   ```toml
   # deny.toml
   [[bans.deny]]
   name = "egui"
   wrappers = ["snuffler-web-egui"]   # + "snuffler-web-leptos" if it ever uses egui bits
   ```

   plus a `task lint` step running `cargo deny check bans`. (A lighter alternative is a
   taskfile assertion that `cargo tree -p snuffler-web-core -e normal` contains no
   `egui`, but cargo-deny states the rule declaratively and covers `eframe`/`epaint`
   with sibling entries.)

Signature hygiene follows from the same rule: because core cannot name egui types, the
repaint waker (finding 1) and the `DrawTarget` trait are forced to stay in toolkit-neutral
vocabulary — the type system keeps the seam honest over time.

Shared geometry/color vocabulary is *not* a leak: `emath` and `ecolor` are standalone
math/color crates (egui re-exports their `Pos2`, `Rect`, `Vec2`, `Align2`, `Color32`).
Core and render may depend on them without pulling any UI toolkit; they are the project's
"std for 2-D geometry", not egui.

## How much a leptos frontend would really share (honest accounting)

**Shared as-is (after the extraction below):**

- `snuffler-web-proto` + the whole Python/Pyodide/worker stack — untouched.
- The bridge: transport, RPC dispatch, SeedLink client + framing, view/figure/tile
  caches, generation counters. Toolkit-free once the repaint waker lands.
- `AppState` + every settings enum, marker model + `:command` parsing, time-axis math,
  track geometry/layout math.
- **The waveform renderer** (`snuffler-web-render`). This is the big one. The
  `DrawTarget` trait (`view/draw.rs`) already abstracts painting — `EguiTarget` and
  `SvgTarget` prove the seam with two real backends. A DOM frontend renders the waveform
  to a `<canvas>` anyway (16k points and 400k waterfall cells are not DOM material), and
  `CanvasRenderingContext2d` maps 1:1 onto `DrawTarget` (`fill_rect`, `line`,
  `polyline`, `text`, `measureText`, clip save/restore). The hardest ~2000 lines of UI
  code are reusable, not rewritten.
- The frame-driver logic ("session sync"): drain bridge generations into state, drain
  state flags into bridge calls, compute `ViewParams`. Today interleaved inside
  `App::ui`; extracted, both frontends call the same methods.

**Rewritten per frontend (inherently, and that's fine):**

- Menus, dialogs, forms, control panel, marker table, snuffling panels — the widget
  chrome. In egui that is roughly `app.rs`'s layout code + `controls.rs` + the
  `snufflings/*::panel` functions (~2500–3000 lines). The leptos version of this surface
  in HTML/tailwind is the whole point of a DOM frontend.

**Shareable with refactoring effort (defer until leptos work actually starts):**

- Pointer/keyboard interaction on the waveform canvas (`handle_interaction`,
  `handle_keyboard`, ~700 lines). The pan/zoom/pick math is toolkit-independent, but
  today it reads `egui::Response`/`InputState` directly. When needed, refactor to
  functions taking plain event data (position, modifiers, wheel delta) with a thin
  egui/DOM mapping at each edge. Do not build this mapping before there is a second
  consumer.

**Reactivity mismatch, resolved at the edge:** leptos wants signals; egui wants a plain
mutable struct. Keep `AppState` a plain struct in core. The leptos crate wraps it at its
edge (`reactive_stores::Store` or a coarse `RwSignal` + rAF-driven canvas redraw); the
egui crate keeps mutating it directly. No signals, no `#[derive(Store)]`, no feature
flags in core.

## Target workspace shape

Laid out as if `snuffler-web-leptos` already existed: each frontend entry crate is
self-contained (its own `index.html`, `Trunk.toml`, frontend-specific assets like the
Ubuntu font), and everything both frontends consume lives outside them.

```
Cargo.toml                       # virtual manifest: [workspace] only, no [package]
web/                             # shared JS/TS glue + Pyodide worker (both frontends)
proto/                           # shared protobuf schema (both frontends + Python)
crates/
  snuffler-web-proto/            # renamed from snuffler-proto: generated protobuf types
  snuffler-web-core/             # NEW: the data layer (lib, builds for wasm + host, NO egui)
    src/
      state.rs                   # AppState + settings enums (from src/state.rs)
      session.rs                 # NEW: Session driver (state + bridge + sync + intents)
      bridge/                    # from src/bridge.rs + bridge/proto.rs (Repaint waker, no ctx)
      seedlink.rs                # framing/selector logic, compiled on ALL targets, host-tested
      markers.rs                 # Marker model + :command parsing (from view/markers.rs)
      timeaxis.rs                # from view/timeaxis.rs (already pure)
      snufflings.rs              # per-snuffling Config + params + Action (no panels)
  snuffler-web-render/           # NEW: the waveform renderer (lib, NO egui)
    src/
      draw.rs                    # DrawTarget trait + SvgTarget (deps: emath + ecolor)
      palette.rs                 # wiggle themes + own two-variant Theme enum
      layout.rs                  # track_geometry / compute_layout
      paint.rs                   # the paint pipeline against DrawTarget
      waterfall.rs               # waterfall image building (raw RGBA out, no textures)
  snuffler-web-egui/             # the egui frontend (bin) — self-contained entry point
    index.html                   # moved from repo root
    Trunk.toml                   # moved from repo root
    assets/fonts/                # Ubuntu-Regular.ttf (egui-specific)
    src/
      main.rs, app.rs, view/*    # widgets, windows, panels, EguiTarget, texture caches
  snuffler-web-cors-proxy/       # renamed from snuffler-cors-proxy
  snuffler-web-tcp-ws-proxy/     # renamed from snuffler-tcp-ws-proxy
  snuffler-web-proxy/            # renamed from snuffler-proxy
  snuffler-web-leptos/           # FUTURE — its own index.html + Trunk.toml + tailwind setup
```

Dependency graph (arrows = "depends on"):

```
snuffler-web-egui ──► snuffler-web-render ──► snuffler-web-core ──► snuffler-web-proto
        │                                          ▲
        └──────────────────────────────────────────┘
(snuffler-web-leptos, later, gets the identical shape)
```

- `snuffler-web-core`: depends on proto, `emath`, `ecolor`, and the wasm-bindgen family
  (transport, `cfg(target_arch = "wasm32")`) — **never** on egui/eframe/epaint.
- `snuffler-web-render`: depends on core (it draws `AppState` + `ViewData`), `emath`,
  `ecolor`. Render is "how a session is drawn"; core is "the session" — core never knows
  render exists.
- Frontend crates depend on core + render, never the reverse (cargo forbids the cycle).

Bridge and state deliberately share the core crate rather than splitting further: they
co-change on nearly every feature (see git history), and the rule worth enforcing —
"no egui" — is enforced by the core crate boundary already. One `Cargo.toml` fewer.

## Findings, ranked

### 1. `Bridge` takes `&egui::Context` in ~35 methods (repaint waker in disguise)

`src/bridge.rs` uses the context exclusively for `request_repaint()` /
`request_repaint_after()` (40 call sites) and one `ctx.input(|i| i.time)` clock read.

- **Drift scenario:** every new bridge method copies the `ctx` plumbing; a second
  frontend is impossible without touching all of them; none of the bridge's dispatch
  logic can be unit-tested off-browser because the signatures demand an egui context.
- **Fix:** inject a waker at construction and delete the parameter everywhere:

  ```rust
  /// Wakes the frontend when async results land (egui: request_repaint;
  /// a DOM frontend: schedule a requestAnimationFrame redraw).
  #[derive(Clone)]
  pub struct Repaint(Rc<dyn Fn()>);

  impl Repaint {
      pub fn new(f: impl Fn() + 'static) -> Self { Self(Rc::new(f)) }
      pub fn request(&self) { (self.0)(); }
  }

  pub struct Bridge { inner: Rc<RefCell<Inner>>, repaint: Repaint }
  ```

  `App::new` builds it as `Repaint::new(move || ctx.request_repaint())` from the
  `CreationContext`'s context. The few `request_repaint_after(d)` sites either keep a
  second closure taking a `Duration`, or — simpler — the app converts them to a plain
  `request()` plus its existing per-frame polling; decide at implementation time, both
  are fine. The one `ctx.input(|i| i.time)` (seisbench progress-poll pacing) becomes
  `js_sys::Date::now()` (already used elsewhere in the crate).
- **Payoff today:** removes a parameter from ~35 methods and their ~60 call sites in
  `app.rs` — net *negative* boilerplate — and is the single change that makes the bridge
  extractable. Once core is a crate, the manifest makes this seam permanent: core cannot
  even name `egui::Context` again.
- **Effort:** hours; fully compiler-driven.

### 2. Workspace layout: root is both workspace and package

The root `Cargo.toml` is workspace + the `snuffler-web` package with a top-level `src/`.
Requirement: virtual manifest + everything under `crates/`, `snuffler-web-` prefix on
every crate. No backward compatibility to preserve — move and rename freely; the single
invariant to protect is the Pages deployment.

- **Drift scenario:** with one package there is no compiler-enforced boundary at all —
  anyone can `use egui` from `state.rs` and nothing fails. Also blocks per-crate feature
  hygiene (today `svg-raster`/`threads`/`gf` all live on the one package).
- **Fix:** root becomes `[workspace]`-only; the app package moves to
  `crates/snuffler-web-egui` and *owns its whole web entry*: `index.html`, `Trunk.toml`,
  and the egui-specific assets (fonts) move into the crate directory. That is the shape
  a second frontend needs anyway — two frontends cannot share one root `index.html`, so
  don't leave it there for one. Mechanics that move in the same change:
  - Root `Cargo.toml`: keep `[workspace]`, `[workspace.dependencies]`,
    `[workspace.metadata.cargo-fc]`, `[profile.release]` (profiles are workspace-root
    only). Move `[package]`, `[features]`, `[dependencies]`, `[lints]`,
    `[package.metadata.cargo-fc]` (the wasm32 target + `threads` exclusion) into
    `snuffler-web-egui`. Lift the `[lints]` block to `[workspace.lints]` + per-crate
    `lints.workspace = true` while touching it, so core/render/egui/proxies stay in
    lockstep.
  - **Crate renames:** `snuffler-proto` → `snuffler-web-proto`, `snuffler-proxy` →
    `snuffler-web-proxy`, `snuffler-cors-proxy` → `snuffler-web-cors-proxy`,
    `snuffler-tcp-ws-proxy` → `snuffler-web-tcp-ws-proxy`. Ripple points to update in
    the same commit: `taskfile.yaml` `-p` flags, inter-crate `path` deps,
    `micromux.yaml` (runs the `snuffler-proxy` binary), README, CI workflows.
  - **Trunk moves into the crate.** `trunk serve`/`build` run against
    `crates/snuffler-web-egui/Trunk.toml` (either `cd` in the mise tasks or
    `--config crates/snuffler-web-egui/Trunk.toml`). The hooks currently assume the
    repo root as working directory (`buf generate` reads root `buf.yaml`; `bun build
    web/bridge.ts` uses root-relative paths) — rewrite them against
    `$TRUNK_SOURCE_DIR/../..` or have the hook `cd` to the repo root first. Same for
    `index.html`'s references to `web/gen/*`, `sources/pyrocko` test data, and
    `scripts/trunk-post-build.sh` staging paths.
  - **Bin/artifact naming:** rename the bin to whatever fits (`snuffler-web` is still a
    fine artifact name); just remember `scripts/trunk-post-build.sh:74` greps
    `snuffler-web-*.js` to patch the wasm-bindgen-rayon worker import — update the glob
    to match whatever the bin is called, or the threaded build's worker bootstrap
    silently breaks.
  - **Pages invariant:** the deploy workflow uploads Trunk's `dist/`. Set `[build]
    dist` in the moved Trunk.toml (repo-root `dist/` or crate-local — either is fine)
    and point `.github/workflows/pages.yml`'s upload step and
    `docker/pyodide-mt/stage-web.sh`/post-build staging at the same location. Verify
    with a full `mise run build` + a served `dist/` smoke test before merging.
  - `.cargo/config.toml` (wasm rustflags, aliases) is path-independent — unchanged.
- **Effort:** about a day including CI verification (`task lint:fc`, `mise run build`,
  Pages workflow dry-run on a branch).

### 3. The egui-free data layer is interleaved under egui-owned files

Today's misplacements, each a small move into `snuffler-web-core`:

- `view/markers.rs` holds the **Marker domain model** and `:markers` command parsing —
  referenced by `state.rs`, i.e. the domain model lives under the presentation tree.
  Its only egui import is `Color32` (= `ecolor::Color32`). Move; keep the marker-table
  *widgets* frontend-side.
- `view/timeaxis.rs` is already pure math. Move as-is.
- **SeedLink framing is locked inside the `cfg(wasm)` bridge module and therefore
  untestable on host** — `is_seedlink_header`, `find_seedlink_header`,
  `drain_seedlink_records`, `seedlink_v4_selectors` are pure byte/string functions
  implementing a wire protocol. A partial-record-across-websocket-frames bug is exactly
  the class of defect host unit tests catch and browser testing misses. Move to
  `core::seedlink`, compiled for all targets, with tests. This is the highest-value
  *testability* win in the review.
- `snufflings/*`: each file mixes an egui-free `Config` (+ `params()` → proto) with an
  egui `panel()`. Config/params/`Action` move to core; panels stay. The `Action` enum is
  already toolkit-free — it is the right shape for both frontends.
- `state.rs` moves wholesale (already egui-free). Taste note, not a finding: the flat
  form-buffer fields (`dl_*`, `cat_*`, `live_seedlink_*`) would read better grouped into
  `DownloadForm`/`CatalogForm`/`SeedlinkForm` substructs; both frontends need these text
  buffers, so they stay in shared state either way.
- **Effort:** 1–2 days, mostly `git mv` + import fixes; compiler-driven.

### 4. The waveform renderer deserves its own crate — extract it now, not with leptos

`view/waveform.rs` (1949 lines) contains the pure pipeline (palette, `track_geometry`,
`compute_layout`, `paint` against `DrawTarget`) interleaved with the egui-coupled parts
(`show`, scrollbar, `handle_interaction`, `handle_keyboard`). The pipeline is the single
most valuable shared asset for a second frontend *and* the code whose correctness is
hardest to eyeball — it deserves the same crate-level egui ban as core.

- **Fix:** extract `snuffler-web-render`: `DrawTarget` + `SvgTarget` (from
  `view/draw.rs`), palette (with its own two-variant `Theme` enum; the egui crate maps
  `egui::Theme` into it), layout/geometry, `paint`, and waterfall image building
  (`WaterfallCache::build_image` returns raw RGBA + dims instead of `egui::ColorImage`,
  so image *construction* is render-side and texture *upload* stays frontend-side).
  `EguiTarget` and everything reading `egui::Response`/`InputState` stays in
  `snuffler-web-egui`.
- **Payoff today:** SVG export logic and paint/layout math become host-testable
  (goldens for tick layout, geometry, SVG output are all possible off-browser);
  the egui crate shrinks to widgets + input handling; and when the leptos crate
  arrives, its `CanvasRenderingContext2d` `DrawTarget` impl slots in with zero changes
  here.
- **Effort:** ~1 day, done as part of (or immediately after) finding 3. Defer only the
  interaction-event abstraction (see the honest accounting above) until leptos starts.

### 5. `app.rs` is a 2284-line monolith; `fn ui` is ~1100 lines with a `too_many_lines` allow

Co-change evidence: `app.rs` + `bridge.rs` + `state.rs` + `view/waveform.rs` appear
together in essentially every feature commit. Some of that is inherent (a feature = a
setting + a request + a widget), but `App::ui` interleaves three separable jobs:

1. **Sync**: drain bridge generations (scene, goto, command feedback, snuffling results,
   figures) into state; drain state flags (`reload_requested`, `set_active_event_request`,
   watch-file polling) into bridge calls; compute and submit `ViewParams`.
2. **Layout**: panels, menus, windows.
3. **Feature dialogs**: marker table (with its cell-edit helpers), download/catalog
   forms, help/about.

- **Drift scenario:** the sync logic is exactly what a second frontend must replicate;
  interleaved with layout, it can only be shared by copy-paste, which then diverges
  (e.g. one frontend forgetting the `scene_gen` reset-view rule).
- **Fix (structural):** extract a `Session` into core:

  ```rust
  pub struct Session {
      pub state: AppState,
      pub bridge: Bridge,
      // generation cursors currently on App: last_scene_gen, last_snuffling_gen,
      // last_active_event_gen, last_scene_bounds, last_watch_poll, …
  }

  impl Session {
      /// Once per frame/tick: apply everything that arrived from the bridge,
      /// flush pending intents from state, keep the view request current.
      pub fn sync(&mut self, now_s: f64, visible_bins: u32) { … }
      pub fn view_params(&self, npx: u32) -> ViewParams { … }
      pub fn run_command(&mut self, line: String) { … }  // markers intercept + dispatch
      // intent methods: load_file, start_seedlink, set_active_event, …
  }
  ```

  The egui `App` shrinks to: `session.sync(...)`, draw widgets that mutate
  `session.state` directly (immediate-mode stays immediate-mode — **no** action enum for
  ordinary mutations), call session intent methods from buttons. Texture caches
  (`FigureCache`, waterfall textures, map tiles→textures) stay on the egui side; they
  are GPU-resource caches, inherently per-frontend.
- **Fix (local, do it while extracting):** marker-table window + `edit_num_cell`/
  `edit_text_cell`/`sort_header` → `view/marker_table.rs`; download/catalog/seedlink
  forms → `view/forms.rs`; help/about text → `view/help.rs`. The `too_many_lines` allow
  should become unnecessary.
- **Effort:** 2–4 days, incremental — move one drained signal at a time; every
  intermediate state compiles.

## What NOT to do (anti-boilerplate guardrails)

- **No `snuffler-web-leptos` crate now.** An empty shell rots. The workspace shape,
  the enforcement, and the render crate are the preparation; the crate appears when the
  work starts and touches nothing outside its own directory.
- **No Elm-style action/message enum for state mutation.** egui mutates `AppState`
  directly and that is correct for immediate mode; forcing message-passing would add
  hundreds of lines for zero enforcement. Intent methods on `Session` exist only where
  a mutation must touch the bridge or cross-cut (commands, event activation).
- **No UI-toolkit abstraction traits** (`trait Widget`, `trait Dialog`, view-model
  layers). The chrome is per-frontend; the seams are `Session`, `Repaint`, and
  `DrawTarget`, nothing higher.
- **No DTO mapping layers.** Decoded prost messages remain the app-facing model
  (existing decision, documented in `bridge.rs` — keep it).
- **Don't move `AppState` behind signals/stores in core.** Leptos wraps it at its own
  edge.

## Migration route (each step compiles, ships, and is safe to stop after)

1. **Repaint waker** (finding 1). Behavior-preserving; compiler enumerates all call
   sites. Verify: `task lint:fc`, run the app, confirm async results still repaint
   (file load, figure windows, seedlink).
2. **Workspace move + renames** (finding 2). Root goes virtual; app package →
   `crates/snuffler-web-egui` with its `index.html`/`Trunk.toml`/fonts; existing crates
   gain the `snuffler-web-` prefix; taskfile/micromux/CI/scripts updated. No Rust code
   changes. Verify: `mise run build` end-to-end including the threaded build (the
   post-build worker patch grep must match the bin name) and a Pages-workflow dry-run
   on a branch.
3. **Extract `snuffler-web-core`** (finding 3): state, markers, timeaxis, seedlink
   (+ host unit tests for framing), snuffling configs, bridge. Add `-p
   snuffler-web-core` to the host-side taskfile targets so its tests run on native CI.
   Land the cargo-deny bans (enforcement section) in the same step, so the boundary is
   guarded from day one.
4. **Extract `snuffler-web-render`** (finding 4): draw/palette/layout/paint/waterfall
   image building, host-tested; `EguiTarget` + input handling stay in the egui crate.
5. **Extract `Session`** (finding 5), one drained signal at a time; split the app.rs
   dialogs into modules as they detach.
6. **Later, when actually wanted:** `crates/snuffler-web-leptos` — its own
   `index.html` + `Trunk.toml` + tailwind pipeline next to the egui crate's; DOM chrome
   in leptos, waveform on `<canvas>` via a `CanvasRenderingContext2d` `DrawTarget` impl
   (or a WebGL2 renderer if profiling demands it), `Repaint` mapped to a dirty-flag +
   `requestAnimationFrame`, interaction math extracted from the egui handlers into
   core/render as the mapping is written. The Pyodide worker, `web/` glue, and proto
   stack are shared unchanged.

Steps 1–5 are justified by the current app alone (testability, enforcement, smaller
files, the workspace layout); step 6 inherits a boundary that by then already exists,
is CI-guarded, and is exercised by two render backends (egui + SVG).
