# Deep-review implementation progress

Companion to `python-task-lifecycle-and-marker-scalability-deep-review.md`.
The review document is immutable; this file records implementation decisions and verification.

## Target

- Starting head: `de1723205bf6b2c5d6455eaa704d078ff390d7b8`
- Implementation status: complete
- Eframe fork: commit `2e34b10e` published and pinned by the snuffler-web lockfile
- Snuffler-web commits: none requested; changes remain uncommitted

## Workstreams

| Workstream | Status | Evidence |
| --- | --- | --- |
| Lint enforcement and flow ownership | complete | Explicit flow operations and exact lane slots; core wasm cargo-fc rows added |
| Await-safe cancellation and typed errors | complete | External-await states cover JS promises; outer `BaseException` envelope added |
| Marker producer, side-table, and store bounds | complete | Producer accumulators, compact NSLCs, bounded event projection, and store cap |
| Paged browser file ingestion | complete | 32 MiB/64-item pages, oversized-file rejection, explicit flow cancellation |
| Request deadlines, boot failure, and runtime-loss cleanup | complete | Read deadlines, eager boot failure event, centralized derived-slot cleanup |
| SeedLink bounds, sequencing, and transactional presentation | complete | Pre-copy and aggregate bounds, admission rollback, gap sequencing, deferred presentation reset |
| Nonblocking marker/export preparation | complete | Painted activity before egui SVG preparation; rayon raster and marker preparation |
| Full regression and browser validation | complete | Feature matrix, unit, parity, and export-aware browser checks green |
| Bridge snapshot ownership | complete | `Bridge::view` returns `Arc<ViewData>`; no public API exports an `Inner` borrow guard |
| Epaint wasm lock investigation | complete and integrated | Browser-wasm debug contention fails immediately without clocks or parking; fork commit `2e34b10e` is published and pinned |
| Branch-wide naming audit | complete | Lifecycle states, session transactions, activity metadata, paged file loads, live ingest, marker summaries, and worker helpers now expose their actual roles |
| Branch-wide duplication and drift audit | complete | Protobuf reflection, shared atomic states, typed lifecycle predicates/conversions, and single settlement/marker-table helpers replace copied encodings |
| Cross-language type-hardening audit | complete | Protobuf progress/runtime/device DTOs, one shared worker protocol, discriminated source types, and explicit lifecycle/export outcomes replace optional-field and boolean state encodings |

## Decisions

- Preserve the single activity ledger, two Python lanes, worker protocol assertions, and Python
  workspace transaction.
- Introduce explicit flow operations for composite browser actions instead of borrowing atomic RPC
  metadata.
- Keep the original deep-review document unchanged; implementation discoveries and deviations live
  here.
- Make each intermediate state compile and keep behavioral repairs separate from organizational
  moves.
- Store the current waveform view as one immutable `Arc<ViewData>` and return owned snapshots. This
  makes bridge mutation while rendering or exporting safe by construction without copying samples.
- Keep egui text measurement on the main browser thread. A cloned `egui::Context` shares its locks;
  no synchronous mutex implementation can safely park the browser main agent while a worker owns
  the lock.
- Do not add a detached SVG worker pipeline without a measured need. It requires a second bounded
  marker representation plus render-settings and font snapshots; the real maximum-budget Eifel
  regression keeps the bounded SVG stage below 500 ms and proves rasterization runs on rayon.
- Name execution-control states by the job phase plus interpreter ownership or external-wait
  condition. A job can execute while it lacks interpreter ownership, so `WithoutInterpreter` is
  accurate where `Suspended` was not.
- Keep activity metadata explicit about its consumer: `session_access` controls Python
  transactions, `secondary_feedback` controls navbar priority, `cancellation` describes support,
  and `may_update_markers` gates destructive marker edits.
- Keep the execution-control word out of protobuf. It is a request-local `SharedArrayBuffer`
  synchronization protocol used only by two TypeScript modules, not a Rust/JS/Python DTO; the
  exported `ExecutionControlState` enum is its single encoding and the transition tests import it.
- Decode activity options and `BridgeResult` mappings with build-only protobuf reflection. The
  `.proto` descriptors now supply field tags, enum values, method metadata, and oneof variants;
  the Rust generator maintains no parallel ordinal or response table.
- Keep `_DatasetState` capture, transaction clone, and install assignments explicit. Their copy
  depths intentionally differ; a reflective string-key table would shorten the file while making
  scientific object ownership less checkable.
- Keep the marker cap enforced independently by producer and consumer. It is a two-sided resource
  bound, whereas marker protobuf construction and NSLC side-table rebuilding now each have one
  implementation.
- Express the two supported marker-result protobufs with constrained `_MarkerResultT`. The
  accumulator, event finalizer, and truncator now preserve same-type-in/same-type-out without
  parallel `@overload` declarations.
- Represent operation progress with a protobuf `oneof`: every event is either count progress or a
  normalized fraction, never an ambiguous combination of optional fields.
- Carry runtime capabilities and the active ML device as protobuf bytes. Worker replies and events
  use a shared TypeScript protocol module, while browser file sources use a discriminated union that
  makes the handle-only operations compile-time safe.
- Use named enums for append-versus-replace source loading, per-file and per-page outcomes, live
  ingest completion, cancellation status, and marker export scope. These states no longer depend on
  booleans whose meaning changes by call site or invalid `Option` combinations.
- Generate literal Python activity lane and session-access types from protobuf metadata. Keep `Any`
  only at genuinely dynamic Pyodide and untyped pyrocko boundaries.
- Preserve every SVG waveform point and the established hundredth-pixel precision while using a
  dedicated finite-float serializer. This removes the intermediate per-polyline string and keeps
  large PNG preparation below the browser responsiveness bound without moving egui font access to
  rayon.

## Validation log

| Check | Result |
| --- | --- |
| Baseline `task lint:fc` | failed: cfg-gated `Operation` import and unscoped bootstrap spawn |
| Baseline core cargo-fc matrix | 15 rows, host triples only; no wasm primary row |
| Updated core cargo-fc matrix | 20 rows including `wasm32-unknown-unknown` primary rows |
| `task check:ts` | passed; external-await execution-control checks green |
| `task check:py` | passed; mypy clean after bounded marker producers |
| `task check` | passed after paging and commit-boundary changes; one SeedLink mutability warning subsequently fixed |
| `task lint:fc` | passed: 35 feature/target combinations; expected unstable-`atomics` warning only |
| `task test` | passed: 129/129, including aggregate SeedLink and cumulative marker-cap regressions |
| `task test:parity:bridge` | passed: 41/41; producer bounds and total `BaseException` boundary covered |
| `task test:a11y:browser` | passed: 37/37, including Eifel PNG, SVG, repeat export, and post-export interactivity |
| PhaseNet `task test:seisbench:browser` | passed on WebGPU and CPU wasm; 10 typed P markers on both paths |
| Final `task check` | passed: Rust host/threaded wasm, mypy, TypeScript, execution-control state machine |
| Final `task lint:fc` | passed after formatting: all feature/target, boundary, threaded wasm, and Python checks |
| Epaint `cargo fmt --all --check` | passed with the wasm lock conditional |
| Epaint wasm `cargo check -p epaint --target wasm32-unknown-unknown --all-features` | passed |
| Epaint release wasm check | passed with `--all-features --release` |
| Epaint native unit suite | passed: 36/36, including the native debug contention diagnostics |
| Owned-view `task check` | passed for host, threaded wasm, Python, TypeScript, and execution control |
| Hardened `task test:a11y:browser` | passed: 40/40; isolation, rayon execution, frame progress, PNG/SVG, and post-export interaction proven |
| Naming-audit `task test:parity:bridge` | passed: 41/41 after Python transaction and generated protobuf renames |
| Naming-audit `task test` | passed: 130/130 |
| Naming-audit `task lint:fc` | passed: 35 feature combinations and threaded wasm check |
| Naming-audit browser regression | passed: 40/40 against the rebuilt served artifact |
| DRY-audit `task check` | passed: descriptor-driven Rust/Python/TypeScript codegen and shared execution-control states |
| DRY-audit `task test` | passed: 130/130 |
| DRY-audit `task test:parity:bridge` | passed: 41/41 after the marker fixture moved to the single side-table rebuild path |
| DRY-audit `task lint:fc` | passed: 35 feature combinations, boundary checks, threaded wasm, and Python lint |
| DRY-audit browser regression | passed: 40/40 on the freshly rebuilt distribution, including Eifel and repeated PNG/SVG export |
| Type-hardening `task check` | passed: native Rust, threaded wasm, mypy, TypeScript, and execution-control checks |
| Type-hardening `task lint:fc` | passed: 35 feature combinations, dependency boundaries, threaded wasm, and Python lint |
| Type-hardening `task test` | passed: 131/131, including cancellation-status and SVG-coordinate precision regressions |
| Type-hardening `task test:parity:bridge` | passed: 41/41 after exact marker protobuf annotations |
| Type-hardening browser regression | passed: 40/40 against the rebuilt app, including Eifel, delayed wiggle feedback, repeated PNG/SVG export, interactivity, and no console errors |
| Type-hardening PhaseNet browser regression | passed on WebGPU and CPU wasm; protobuf device status and fractional progress decoded correctly |
| Type-hardening `task typos` | passed |
| Export-tail browser regression | passed three times consecutively at 40/40 after exact SVG serialization was optimized; Eifel PNG kept painting, repeated PNG/SVG exports completed, and no console errors appeared |

## Discoveries and deviations

- Browser file enumeration is represented by an opaque, ephemeral flow ID and pulled page by page;
  the protobuf remains the sole cross-boundary DTO.
- A successful empty-scene or first SeedLink reset acknowledgement now triggers the frontend
  annotation reset. Validation/connection/admission failures preserve the existing presentation.
- Strengthening cargo-fc exposed pre-existing wasm-only lint debt in the bridge. The job runner,
  file-page application, and SeedLink acknowledgement paths were split into named helpers rather
  than suppressing the diagnostics.
- Egui text measurement remains on the browser main thread. Its debug lock timeout reaches an
  unsupported `std::time::Instant` when a rayon worker contends for the context, so rayon receives
  only the owned SVG string for worker-safe resvg rasterization.
- Pinned egui/epaint has no clock feature that changes this boundary. Eframe uses `web-time`, while
  epaint's debug `parking_lot::try_*_for` path still depends on `std::time::Instant`; `egui/rayon`
  enables parallel tessellation but does not replace that clock.
- SVG activity completion can safely happen while the owned view snapshot remains live. The bridge
  no longer exports a `RefCell` guard, so the former scope-ordering requirement has been removed.
- The follow-up multi-model review converged on the owned `Arc<ViewData>` seam and found no other
  public bridge method returning an `Inner`-tied guard or reference.
- The same review found that replacing timed epaint locks with ordinary `parking_lot` locks is not a
  general license to share `egui::Context` with rayon. Without `parking_lot/nightly`, contended wasm
  parking panics; with its atomic parker, a contending browser main agent still cannot synchronously
  wait. The fork therefore uses an immediate debug diagnostic on browser-wasm contention, and the
  application moves only owned render data to workers.
- If SVG construction later exceeds the browser bound, the reviewed escalation path is an owned
  render-settings snapshot, at most 2,000 cloned visible markers (or only the overload summary), an
  `Arc<ViewData>`, and a worker-local epaint font engine built from cloned font definitions. It must
  never clone the live `egui::Context` or all of `AppState`/`MarkerStore`.
- A post-refinement Chromium run caught a 594 ms tail in exact SVG coordinate serialization. The
  renderer was still formatting and copying hundreds of thousands of coordinates on the main
  thread before rayon rasterization began. Replacing only that formatter retained all geometry and
  precision; three consecutive rebuilt-browser runs then passed the sub-500 ms responsiveness gate.
- `prost-build` exposes the compiled descriptor set but deliberately drops unknown extension data
  when it decodes into `prost_types`. `prost-reflect::DescriptorPool::decode` preserves those custom
  method options, so it removes the former hand-written descriptor and `Activity` shadow structs
  without adding anything to the shipped wasm.
