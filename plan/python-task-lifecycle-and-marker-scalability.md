# Python task lifecycle and marker scalability

## Verdict: local maximum — this document is the target design

The Python/Pyodide worker boundary and protobuf transport are sound. The operation-lifecycle
model and high-cardinality marker rendering are not: they rely on a singleton status, scattered
`in_flight` flags, incidental worker serialization, and unbounded main-thread work. Adding more
`Status::Loading(...)` assignments would only polish the current design.

This revision is the product of a multi-model adversarial review (three independent reviewers plus
orchestrator verification, two rounds, every claim checked against source). It replaces the earlier
draft's federated two-queue target with a single-owner design, adds the session-transaction and
safe-point model that makes limited concurrency (one view RPC + one long job) sound, and specifies
the type-level enforcement so an implementor cannot drift from it.

## Domain purpose

Snuffler Web lets a seismologist inspect dense waveform datasets, run analyses, and turn their
results into traces, figures, and markers while continuing to navigate and edit the current
dataset. Each analysis should have one observable lifecycle, operate on a well-defined session
snapshot, and either commit a complete result or fail visibly. Annotation count must not determine
whether the interface remains usable.

## Reproduced behavior

The Eifel example contains 1,061 traces and 717 catalogue markers. Opening the marker editor:

- Dropped the displayed rate from 60 fps to 10–11 fps.
- Expanded the accessibility DOM from 44 positioned widgets to 12,360 current DOM nodes.
- Caused 11,549 node descriptions and 23,109 placements in its opening updates.

The direct cause is that the supposedly scrolling table constructs every row and all 16 cells every
frame and re-sorts the full permutation every frame
(`crates/snuffler-web-egui/src/view/marker_table.rs:76-171`; plain `ScrollArea` + `Grid`, no
`show_rows`). The accessibility layer accurately mirrors that unbounded widget tree; accessibility
is an amplifier, not the bug. Understated in the earlier draft:

- Hover hit-testing runs **twice per frame** (`view/waveform.rs:337` crosshair, `:592`
  `hovered_marker`) plus once per press (`:717`), and each pass scans every marker calling
  `marker_row_spans`, which allocates and does `markers × visible_tracks × nslc_ids` glob matches
  (`crates/snuffler-web-render/src/paint.rs:1108-1128`) — for Eifel roughly 760k glob matches per
  hover frame.
- Even the >2000-marker summary fallback (`paint.rs:1307,1329`) iterates all markers twice per
  frame to decide it is overloaded; the overload *check* itself is O(n).
- `bridge.ensure_station_distances(...)` is called every frame the editor is open
  (`marker_table.rs:39-40`).

This is only 717 markers. STA/LTA can return a marker for every detected peak on every channel; a
sensitive run turns the next UI frame into tens of thousands of rows. The large protobuf result is
additionally copied, decoded, cloned (`Bridge::snuffling()` clones the whole result,
`bridge/mod.rs:546-548`), and mapped with four `String` clones per NSLC per marker plus an O(n²)
active-flag clear (`session.rs:428-467`) on the main thread before rendering. That is a credible
explanation for a tab whose reload appeared frozen; the failure path is present and reproducible.

## Threading and interleaving reality (verified)

Pyodide lives in `web/bridge_worker.ts`, not on egui's browser main thread. The worker is a dumb
relay: `self.onmessage` (`bridge_worker.ts:1056`) starts `handleRequest` per message with **no
queue**. Serialization of CPU-bound handlers is *incidental* (they hold the worker's JS event
loop); async handlers interleave at every `await`.

Verified facts the design below builds on:

- 31 RPCs (`proto/snuffler/bridge/v1/bridge.proto:130-162`). Exactly **six** always enter the
  global loading state: `MakeExample` (`bridge/mod.rs:1591`), `EmptyScene` (`:1623`), `LoadBytes`
  direct (`:1662`), `LoadUrl` (`:1693`), `LoadBundle` (`:1810`), `DownloadWaveforms` (`:1919`).
  `RunSnuffling` does so only for SeisBench (`:1878-1882`) yet sets `Ready` unconditionally on
  success (`:1905`); `RmsTraces` sets `Ready` without ever setting `Loading` (`:1956`); catalog,
  station, response, command, and event RPCs never touch status.
- Eleven servicer handlers are `async def` (`web/bridge.py:6170-6426`). Ten await *before* any
  session mutation. Two are order/interleaving-sensitive today: `run_snuffling`'s SeisBench path
  awaits **inside** its mutation window (prologue `_replace_derived("seisbench", [])` at
  `bridge.py:3558` → awaits ONNX inference at `:3648` → commit at `:3711`), and
  `_load_gf_store_url` writes the FS across a sequential `await pyfetch` loop (`:3074-3096`).
  Response loading is completion-order-sensitive: documents append in arrival order and
  resolution prefers the first answering document (`bridge.py:4741-4760`).
- **Two-lane interleaving already happens, unguarded**: a `GetView` posted during a SeisBench run
  is admitted at its awaits and completes atomically today. The user-visible goal of this plan is
  to turn that accident into a checked regime.
- Every snuffling has a **destructive prologue**: it clears its previous derived output before
  computing (`_replace_derived(origin, [])` at `bridge.py:3125` seismosizer, `:3558` seisbench,
  `:3854` stalta, `:6011` rms; commits at `:3387,3711,3923,6075`), each bumping `_DATA_GEN`.
  Atomic for sync handlers; already user-visible for SeisBench; fatal to naive two-lane.
- `get_view` is a sync `def` (`bridge.py:6208`) but not pure: it memoizes `_PILE`/`_CUTOUT_VEC`
  keyed on `_DATA_GEN` (`:143-150, 380-389`), clears/fills `_VIEW_NOTICES`/`_CUTOUT_NOTICES`
  (`:1393-1395, 1919`), and maintains the fix-scale-ranges freeze memory `_OLD_DATA_RANGES`
  (`:2150-2163`). All of that state is **view-lane-owned**: no job-lane handler reads it.
- The worker comment "Python is single-threaded here, so this cannot run *inside* another rpc"
  (`bridge_worker.ts:1017-1021`) is false for async handlers: `py.runPython("bootstrap.warm()")`
  executes inside any suspended handler's await window.
- The interpreter is interruptible: the shipped runtime carries `setInterruptBuffer`
  (`dist/py-mt/pyodide.asm.mjs`), and the page is unconditionally cross-origin isolated
  (`bridge_worker.ts:1-7`), so SharedArrayBuffer-based interrupts and status words are available.
- The renderer already projects the last `ViewData` onto the current `tmin..tmax` at full frame
  rate (`view/waveform.rs::show`); pan/zoom over stale data never freezes. Only data refresh
  stalls — and `ensure_view` refuses to refresh at all unless `Status::Ready`
  (`bridge/mod.rs:2946`), which is the actual freeze users see during long jobs.
- The transport has no recovery: `worker.onerror` rejects pending but never recreates or nulls
  the worker (`web/bridge.ts:149-153`); there is no `messageerror` handler; `send()`'s pending map
  is unbounded (`bridge.ts:221-252`). The repo already contains a complete reference
  implementation of correct worker recovery — the ORT client (`bridge_worker.ts:540-611`): per-
  request timeouts, phase-aware escalation, setup heartbeat, terminate-and-recreate with the
  singleton nulled, reject-all-exactly-once. The Pyodide worker, which matters more, has none.

## Findings

### 1. P0: The marker pipeline permits unbounded main-thread work

As reproduced above. Local fixes: virtualize the table (`ScrollArea::show_rows`), cache the sort
permutation, index hit-testing, apply the overload policy via an O(1) count. Structural fix: the
`MarkerStore` of the target architecture, with **producer-side bounding** — the cap belongs in
Python before the large response is built, not after three copies (see §Target/MarkerStore).

Additional confirmed defect strengthening the store: the table's inline-edit state is keyed by
`Vec` index (`marker_table.rs:15`), and `apply_snuffling_result` does `retain` + `push`
(`session.rs:413-419, 478`), shifting indices — a result arriving mid-edit corrupts which row is
being edited. Identity must be a stable `MarkerId`, not an index.

### 2. P0: Activity is not a model; it is one mutable status string

`Status` (`bridge/mod.rs:186`) conflates runtime readiness, current work, and the latest failure,
and cannot represent two queued or running operations. Consequences (all confirmed): STA/LTA runs
under "Ready"; the Run button stays enabled (`snufflings/widgets.rs:47-54`) — for SeisBench the
second click is *silently dropped* by the `Ready` gate (`:1873`) rather than duplicated, which is
also a bug; the first of two overlapping operations to finish sets `Ready` while the other runs;
SeisBench progress polling clobbers other labels (`:463`); failures are not attached to the
operation that produced them. The existing `Inner::diagnostic` (`:238-241`, tests `:3055-3082`) is
the correct precedent being generalized, not a new idea.

### 3. P0: Python serialization and backpressure are assumed, not enforced

The main-thread client accepts unlimited pending requests; the worker launches every handler
independently; async handlers interleave over shared globals (see the verified table above). The
warm-up (`bridge.ts:337-345` → `bridge_worker.ts:1018-1030`) is fire-and-forget, invisible, and —
once its synchronous import starts — unpreemptible: no queue priority can ever make foreground
work overtake a *started* warm-up. Priority is meaningful only before dispatch.

### 4. P1: Silent rejection creates permanent spinners

`render_figure` silently returns unless `Ready` (`bridge/mod.rs:1836-1846`); the figure window
records the request anyway (`view/figure.rs:92-95`) and spins forever while generation stays zero
(`figure.rs:153-163`); transport errors are discarded without bumping the generation or storing
the error (`mod.rs:1855-1861`). `run_snuffling` (`:1873-1875`) and `load_gf_store_url` (`:2018`)
silently return too.

### 5. P1: A hung or dead worker has no general recovery path

No timeout, heartbeat, cancellation, or stall state; `worker.onerror` rejects current promises but
never invalidates or recreates the worker; no `messageerror` handling. A worker reaped without an
`error` event leaves promises unsettled forever while `refresh_seisbench_progress`
(`mod.rs:442-468`) keeps repainting at 60 fps and pinning a frozen `Loading` label — a permanent
spinner plus a CPU burn. The ORT client (`bridge_worker.ts:540-611`) is the in-repo reference for
the fix.

### 6. P1: Operations escape any per-RPC activity gate

Long-running operations that are **not** `BridgeService` RPCs and would pass a per-RPC
compile-time metadata gate untouched:

| Operation | Site | Activity today |
| --- | --- | --- |
| `WORKER_RELOAD_SNUFFLINGS` | `bridge_worker.ts:985-1015` | `Loading` (`mod.rs:2336`) |
| `WORKER_WARM_DEFERRED` (SciPy import) | `bridge_worker.ts:1018-1030` | none |
| `WORKER_MOUNT_GF_STORE` | `bridge_worker.ts:1031-1041` | none |
| SVG export (O(markers×tracks), main thread) | `app.rs:1313` | none |
| PNG rasterize fallback (main thread) | `app.rs:390-395` | none |
| `markers_to_file` (O(markers), main thread) | `marker_table.rs:465` | none |
| `save_markers` / `save_waveforms*` | `mod.rs:2412-2474` | none |
| watch poll (every 1 s while enabled) | `mod.rs:2691-2733` | none (by design comment) |

The PNG-with-rayon path additionally uses a **third, competing activity UI** — the modal
`progress_overlay` (`crates/snuffler-web-egui/src/job.rs`, driven at `app.rs:413`) — whose
`Disconnected` branch drops a failed export silently (`app.rs:417-419`).

### 7. P1: Coalescing guards are wrong for writes, and the view lease leaks

- `invalidate_view_request` (`mod.rs:1420-1425`) clears `view_in_flight` while the RPC is still
  executing, so `ensure_view` dispatches a second concurrent `GetView` after any catalog/station/
  event install — "at most one view in flight" is false today. Obsolete and released are
  different states; the lease must survive until the terminal event.
- `map_data`'s completion does `i.map = result.ok()` (`:2643`) — a transient failure wipes the map
  and discards the error; `clear_map_state` orphans the in-flight call the same way.
- `station_dists_gen` is set unconditionally even on error (`:2674-2675`) — the generation is
  marked "fetched" forever with stale data.
- `event_request_gen` is one latest-wins counter shared by three **mutating** RPCs
  (`set_active_event`/`update_event`/`remove_events`, `:2736-2815`) — earlier mutations'
  responses *and failures* are silently discarded.

Rule: latest-wins coalescing is legal only for reads. Typed below (`Effect::Read`).

### 8. P1: Live ingest is not transactional

`first_batch.replace(false)` consumes the reset flag **before** the RPC succeeds
(`mod.rs:1181`); every drained batch spawns independently (`:1385-1418`); Python clears the old
pile *before parsing* the reset payload (`bridge.py:4100-4107`); each success writes
`Status::Ready` (`:1410`). A malformed reset batch destroys the old scene and then fails, and the
next batch appends into a silent gap. There is no ingress byte bound while a long job runs.

### 9. P2: CPU pools are not coordinated

The egui rayon pool is capped at four (`main.rs:36`); no `OMP_NUM_THREADS` is set anywhere for
the Pyodide/OpenMP side. An exacerbator rather than the primary reproduced cause. Fix: one
browser compute budget; set the OpenMP limit before importing numerical modules; benchmark two-
and four-thread STA/LTA.

### 10. P2: Controls stay enabled against state a queued job will overwrite

`Delete selected` / `Clear all` (`marker_table.rs:204-225`) are live while a snuffling that will
reinstall markers is in flight; `apply_snuffling_result` then appears to undo the deletion. The
class is wider than duplicate submission: any control editing state a queued job commits to.

## Target architecture

```text
egui controls / DOM adapters (pickers return typed manifests; no TS-issued RPCs)
        │ submit typed intent            (Submission is #[must_use])
        ▼
ActivityLedger (Rust, the ONLY queue)   crates/snuffler-web-core/src/activity.rs
        ├── admission, dedup, priority, coalescing, bounded depth
        ├── navbar = a fold over the ledger (never a mutable cell)
        ├── JobHandle / JobState / Progress / typed errors
        └── generated Operation metadata (total function, no wildcard match)
        │ at most 1 job-lane + 1 view-lane dispatch outstanding, epoch-tagged
        ▼
worker executor (~lease assertions only; NO queue, NO policy)   web/bridge_worker.ts
        ├── per-lane busy assertion → typed protocol fault on violation
        ├── interrupt buffer + execution status word (SharedArrayBuffer)
        └── epoch-stamped replies and events; one terminal event per job
        ▼
Python SessionStore (bridge.py)
        ├── SessionSnapshot (immutable view of committed state, per job)
        ├── JobWorkspace → SessionDelta, committed synchronously by the dispatcher
        ├── safe points: dispatcher-owned; yield == progress report
        └── view lane: get_view against committed generation; owns its caches
        ▼
staged bounded result application
        └── MarkerStore → generational ids, indexes, bounded commit, virtualized table
```

Module shape: `crates/snuffler-web-core/src/activity.rs` (ledger, job types, operation metadata
consumption), `crates/snuffler-web-core/src/bridge/runtime.rs` (runtime state, epochs, restart),
`crates/snuffler-web-core/src/bridge/jobs.rs` (typed submission wrappers; the raw transport is
private to this module), `crates/snuffler-web-core/src/marker_store.rs`. **There is no
`web/python_queue.ts`**: a worker-side queue was considered and rejected — two queues are two
sources of truth, and every failure trace in review round one exploited the seam between them.
The worker keeps its dumb-relay identity and gains only assertions.

### 1. One queue, one owner

The `ActivityLedger` in Rust owns all queued intent: admission, deduplication, priority,
coalescing, cancellation of queued work, and the queued/running counts the navbar renders. It is
single-threaded (`Rc<RefCell<_>>`), host-testable (the non-wasm path of `bridge/mod.rs` already
compiles and has unit tests), and the only place that can disable a control. It dispatches at
most one job-lane and one view-lane request at a time; `postMessage` ordering makes dispatch
order execution order.

The worker holds **zero backlog**. It keeps a busy-bit per lane and rejects (as a typed protocol
fault, surfaced as a bug, never silently) any message violating one-per-lane. Boot-time
submissions stay `Queued { reason: RuntimeBooting }` in the ledger under bounded depth and are
dispatched only after the runtime reports `Booted(epoch)` — nothing waits inside the worker
against an unbounded pending map.

DOM glue stops issuing RPCs. `openFiles`/`openDirectory`/`reloadFiles`
(`bridge.ts:574-745`) become browser adapters that return typed manifests (names + bytes, or
opaque `HandleId`s for re-readable handles; directory enumeration pulled in bounded pages) to
Rust, which submits the load jobs through the ledger. Each user gesture is one **flow**, and a
flow is *not* an atomic job: a `FlowId` owns the long-lived job-lane scheduling lease and the
per-item progress the navbar shows, while each inner RPC is an ordinary atomic child job with
its own `JobId`, its own commit, its own status word, and its own terminal reply — file flows
deliberately commit per file and continue past per-file failures (`bridge.ts:562-607`), and the
types must not pretend otherwise. Cancelling a flow after partial progress terminates it as
`Stopped { committed, skipped }`, never `Cancelled` (which means "nothing committed" and stays
reserved for atomic jobs). The flow is created *before* the picker opens, so browser-adapter
failures (picker denied, unreadable file) have a flow to attach to. The lease means the activity
indicator never flickers off between files and no background job or watch tick can interleave
into the middle of a flow.

Warm-up becomes a real ledger job: `Background` class, submitted by Rust after the first view,
admitted only when both lanes are idle and no flow job is open, visible in the navbar, and
documented as non-preemptible once its synchronous import starts (priority applies only before
dispatch). A user job that arrives behind it shows `Queued`, never a silent wait.

### 2. Operation metadata: a total function, enforced at build time

Declaration lives in the proto, next to each rpc (best locality — a newcomer adding an RPC sees
the requirement at the declaration site):

```proto
// proto/snuffler/bridge/v1/activity.proto
import "google/protobuf/descriptor.proto";
message ActivitySpec {
  string label = 1;                 // non-empty; validated at generation
  enum Lane   { LANE_UNSPECIFIED = 0; VIEW = 1; JOB = 2; }   // an RPC is never Local
  Lane lane = 2;
  enum Policy { POLICY_UNSPECIFIED = 0; LATEST_WINS = 1; DEDUP = 2; QUEUE = 3; EXCLUSIVE = 4; }
  Policy policy = 3;
  enum Effect { EFFECT_UNSPECIFIED = 0; READ = 1; MUTATE = 2; }
  Effect effect = 4;                // LATEST_WINS is rejected unless effect == READ
  bool background = 5;
  bool supersedes = 6;              // dataset-replacing loads: cancel a running analysis first
  bool exclusive_both_lanes = 7;    // reload-class: drains and blocks BOTH lanes (see below)
  enum Cancel { CANCEL_UNSPECIFIED = 0; NOT_CANCELLABLE = 1; COOPERATIVE = 2; INTERRUPT = 3; }
  Cancel cancel = 8;
}
extend google.protobuf.MethodOptions { ActivitySpec activity = 51234; }
```

Policy semantics (admission, defined once):

- `LATEST_WINS` (reads only): a newer submission replaces the queued one; an obsolete in-flight
  reply is discarded by sequence. The coalescing identity is **not** the `Operation` alone — it
  is `(Operation, coalescing key)` where the key is derived from the request by a generated
  per-RPC function (for `GetView` the whole request; for `MapData` the request; a future
  latest-wins RPC with multiple consumers must key by consumer). Two `RenderFigure` requests for
  different figure kinds are different work — `RenderFigure` is therefore `DEDUP` keyed by
  `(kind, params)`, never `LATEST_WINS`.
- `DEDUP`: join an identical queued/running job; key = `(Operation, canonical request bytes)`
  unless the table names a narrower key.
- `QUEUE`: FIFO behind the current job.
- `EXCLUSIVE`: `QUEUE`, plus no other queued mutator may be interleaved ahead of it; with
  `supersedes` it additionally cancels a running analysis (product policy, §3).
- `exclusive_both_lanes` (only `ReloadSnufflings`): admitted only when both lanes are idle and
  no flow is open; while it runs nothing else dispatches — it replaces `bridge.py` itself,
  including `get_view`, and must never execute while any coroutine from the old module is
  suspended (the round-1 reload-during-SeisBench corruption trace).

The Rust-side lane type is a superset of the proto's: `Lane { View, Job, Local }` — `Local` is
main-thread work (exports, rasterization) that no RPC can declare by construction.

Enforcement, using the two generators the repo already has:

- `pybuild/snuffler_py_plugin.py` receives the full `CodeGeneratorRequest` (extensions readable
  via the protobuf runtime) and already hard-fails on unmapped envelope variants; extend it to
  `SystemExit` on a missing/UNSPECIFIED/empty `activity` option, and to emit the metadata tables
  for TS (`bridge_methods.ts`) and Python.
- **Known pitfall, recorded so the implementor does not rediscover it**: `prost_build`'s
  `ServiceGenerator` does *not* surface proto2 extension values. The Rust table is therefore
  produced either by the buf plugin emitting a generated `.rs` file, or by
  `crates/snuffler-web-proto/build.rs` reading the `FileDescriptorSet` with `prost-reflect`
  (which does decode extensions). Either way the generator emits:

```rust
/// Generated: one variant per rpc (WarmDeferred and MountGfStore become RPCs, so they
/// are ordinary generated variants), plus the one irreducible worker control and the
/// hand-listed local ops.
pub enum Operation {
    // BridgeRpc variants (generated from the .proto) …
    MakeExample, EmptyScene, /* … */ WarmDeferred, MountGfStore, GfStoreLabel,
    // The one non-RPC worker control (hand-listed; exclusive_both_lanes)
    ReloadSnufflings,
    // Local main-thread work (hand-listed; the submit_local seam + clippy
    // disallowed-methods make bypassing it a lint failure, not a convention)
    ExportSvg, ExportPng, MarkersToFile, SaveMarkers, SaveWaveforms, WatchPoll,
}
pub struct ActivityMeta {
    pub label: &'static str,
    pub lane: Lane, pub policy: Policy, pub effect: Effect,
    pub background: bool, pub cancel: CancelCap,
}
impl Operation {
    pub const fn activity(self) -> ActivityMeta { match self { /* exhaustive, NO wildcard */ } }
}
```

The generated client's methods reference their `Operation` variant, so an RPC added to the proto
without regenerated metadata fails compilation (same failure mode as the existing
`compile_error!` for a missing envelope variant, `build.rs:110-116`). The `Operation` union — not
a per-RPC-only enum — is what closes Finding 6, with each arm's coverage enforced differently:

- **RPC arms**: generated from the proto; enforcement is the generator + compiler as above.
- **Worker controls**: `web/worker_command.ts` is hand-written TS with no machine-readable form —
  nothing can "generate from" it. Resolution: `WarmDeferred` and `MountGfStore` **become ordinary
  proto RPCs** (`bootstrap.warm()` behind a method; the mount carries an opaque `HandleId` into a
  TS registry the worker resolves before relaying), leaving exactly one irreducible control —
  `ReloadSnufflings`, hand-listed with `exclusive_both_lanes` metadata beside the local ops.
- **Local ops** (`ExportSvg`, `ExportPng`, `MarkersToFile`, `SaveMarkers`, `SaveWaveforms`,
  `WatchPoll`): a hand-listed enum arm forces nothing by itself — a new `spawn_local` or
  `rayon::spawn` simply never touches it. The enforcement seam: the ledger's
  `submit_local(op: Operation, ..)` is the **only** exported wrapper around task-spawning in the
  workspace, backed by clippy `disallowed-methods` entries for `wasm_bindgen_futures::spawn_local`
  and `rayon::spawn` outside `activity.rs`/`bridge/`. Adding local work without an `Operation`
  is then a lint failure, not a convention.

`progress_overlay` (`job.rs`) is deleted; PNG/SVG export and marker serialization run as
`Lane::Local` ledger jobs with the one shared navbar/activity UI.

One deliberate exemption from "the ledger is the only queue": the **map-tile pipeline**
(`mod.rs:2817-2938`) never touches Python — it is a browser fetch+decode cache that is already
bounded (`TILE_BATCH`/`TILE_BATCHES_IN_FLIGHT`), latest-wins by construction (the queue is
replaced wholesale per frame), and epoch-fenced (`tile_epoch`). It is the pattern this plan
wants, pre-existing; routing dozens of per-pan tile fetches through the ledger would be noise.
It stays as is and does not appear in the navbar. The `MapData` vector-overlay RPC, by contrast,
is a job-lane `LATEST_WINS`/`READ` operation like any other. Accepted trade, stated: `MapData`
and `RenderFigure` freshness during a long job is *reduced* versus today's accidental async
interleaving — they now wait for the job's safe points or completion; that is the price of the
checked regime and it is deliberate.

**Complete policy assignment** (step 4 needs all 31 + the controls; the implementor invents
nothing). Default: `lane=JOB, policy=QUEUE, effect=MUTATE, cancel=NOT_CANCELLABLE` — correct for
the quick metadata loads (`LoadMarkers`, `LoadStationXml`, `LoadStations`, `LoadEvents`,
`AddCakeModel`, `LoadBytes`, `MountGfStore`) and the three event mutators (`SetActiveEvent`,
`UpdateEvent`, `RemoveEvents` — serialized FIFO on the job lane, each terminal result applied in
order, which retires their shared latest-wins counter and its discarded-failure defect).
Deviations:

| Operation | Deviations from the default |
| --- | --- |
| `GetView` | lane VIEW, LATEST_WINS, READ |
| `MapData` | LATEST_WINS, READ, COOPERATIVE |
| `ListExamples`, `StationDistances` | DEDUP, READ |
| `MarkersToText`, `GfStoreLabel`, `ExportStations` | READ |
| `ExportWaveforms` | READ, INTERRUPT (a large `pio.save` is long) |
| `RenderFigure` | DEDUP keyed `(kind, params)`, READ, INTERRUPT |
| `RunSnuffling` | DEDUP keyed `(name, params)`, INTERRUPT (superset over its five snufflings) |
| `RmsTraces` | DEDUP keyed `(params)`, INTERRUPT |
| `MakeExample`, `EmptyScene` | EXCLUSIVE, supersedes |
| `LoadUrl`, `LoadBundle` | EXCLUSIVE, supersedes, COOPERATIVE |
| `DownloadWaveforms`, `CatalogSearch`, `FetchStations`, `FetchResponses`, `LoadGfStoreUrl` | COOPERATIVE |
| `IngestMiniSeed` | QUEUE through the §10 one-slot coalescing mailbox |
| `RunCommand` | MUTATE **always** — the command text decides its effect at runtime, so it can never be marked READ |
| `WarmDeferred` | READ, background |
| `ReloadSnufflings` | exclusive_both_lanes |
| local ops (`ExportSvg`, `ExportPng`, `MarkersToFile`, `SaveMarkers`, `SaveWaveforms`, `WatchPoll`) | lane Local; `WatchPoll` background |

### 3. Job types: illegal states unrepresentable

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct JobId(u64);                        // private field; minted only by the ledger
pub struct RuntimeEpoch(u32);                 // bumped on every worker (re)creation

/// EVERY submission mints a ledger entry — including rejected ones, which are
/// born `Done(Failed(Rejected(..)))` and therefore appear in the navbar/history
/// regardless of what the callsite does. Rejection visibility is ledger-owned;
/// `#[must_use]` is a hint, not the load-bearing mechanism (an empty match arm
/// would satisfy it).
#[must_use]
pub enum Submission {
    Accepted(JobHandle),                      // queued or started; state lives in the ledger
    Joined(JobHandle),                        // DEDUP: identical work already queued/running
    Rejected(JobHandle, RejectReason),        // entry already Done(Failed(Rejected))
}
pub enum RejectReason { RuntimeDown(RuntimeFault), QueueFull { depth: usize } }
// RuntimeFault = the non-Ready subset of runtime states; `RuntimeDown(Ready)`
// must be unrepresentable.

pub struct JobHandle { id: JobId, /* no public constructor */ }

pub enum JobState {
    Queued      { position: usize, since_s: f64, reason: Option<QueuedReason> },
    Dispatching { epoch: RuntimeEpoch },      // sent, not yet observed executing
    Running     { epoch: RuntimeEpoch, since_s: f64, progress: Progress },
    Cancelling  { since_s: f64 },
    Done(Outcome),
}
pub enum QueuedReason { RuntimeBooting, BehindJob(JobId) }
pub enum Outcome {
    Succeeded,
    Failed(JobError),                         // attached HERE, never to runtime health
    Cancelled,                                // cancel won before commit; nothing committed
    TooLate,                                  // cancel lost the commit race; result applied
    Stopped { committed: u32, skipped: u32 }, // a FLOW cancelled after partial progress
    Lost(RuntimeEpoch),                       // the worker died mid-dispatch; NOT auto-retried
    Stale,                                    // defensive commit-check fired — a scheduler bug
}
pub enum Progress {
    Indeterminate,
    Count { done: u64, total: Option<u64>, phase: Arc<str> },
    Fraction { fraction: f32, phase: Arc<str> },
}
// Phases arrive over protobuf as owned strings; they cannot be `&'static str`.
// A job also carries `detail: Option<String>` supplied at submission (the file
// name, slug, or model being worked on) so today's dynamic labels
// ("Loading {name}…", mod.rs:1662) survive; `ActivityMeta.label` stays static
// as the generic fallback.

pub enum RuntimeState { Booting(BootProgress), Ready, Restarting, Failed(BridgeError) }
```

`Outcome::Stale` deserves emphasis: under this design it is **unreachable in normal operation** —
the job lane admits one job, views cannot bump the session generation, and ingest drains only at
job-lane idle (§10), so nothing can supersede a running job's snapshot. It exists as a defensive
assertion in `commit(expected_generation, ..)` whose firing indicates a scheduler bug; its test
is fault injection, not a user flow.

`Status` is deleted. Every scattered `in_flight: bool` (`mod.rs:124, 254, 263, 269, 285, 302`) is
deleted. UI slots hold handles, not booleans — e.g.:

```rust
enum FigureSlot {
    Empty,
    Rendering { job: JobHandle, prev: Option<FigureData> },
    Rendered  { data: FigureData, generation: u64 },
    Failed    { error: JobError, prev: Option<FigureData> },
}
```

A spinner can only be rendered from a state that holds a `JobHandle`, and only `submit` mints
handles — "running without an accepted job" does not typecheck. `#[must_use]` on `Submission`
plus deny-warnings makes silent submission a build failure. The navbar renders: runtime state
when not `Ready`; otherwise the highest-priority active job (label from `ActivityMeta`, progress
or elapsed time) and a queued count — computed each frame from the ledger, so it cannot be
clobbered because there is nothing to clobber.

An `EXCLUSIVE` submission while a job runs is **queued with visible state** (never silently
dropped, never invisible — both failure modes of the old design are excluded by the types). A
merely-queued mutator can never invalidate the running job's snapshot (it runs only after the
job's commit), so there is **no safety-motivated auto-cancellation**. There is one *product*
policy, named explicitly: a dataset-replacing load (`EmptyScene`, `LoadUrl`, `LoadBundle`,
`MakeExample`) declares `supersedes: true` in its metadata, and the scheduler cancels a running
analysis job through the ladder before admitting it — because finishing minutes of analysis
whose dataset is about to be replaced wastes the user's time, not because anything would be
unsafe. Removing that flag changes UX, never correctness.

While a job whose metadata declares marker output is queued or running, marker-mutating controls
(Delete selected / Clear all, `marker_table.rs:204-225`) render disabled with the job named —
the ledger is the one place that can disable a control (§1), and this closes Finding 10's
"deletion appears undone" without a merge protocol. (The richer alternative — per-`MarkerId`
reconciliation with deletions winning — is deliberately not chosen; it buys little for a window
of a few seconds.)

### 4. Session transactions (Python)

Module-global mutation from job handlers is retired. `bridge.py` gains a `SessionStore` owning
what is today `_TRACES`, `_DERIVED_*`, `_DATA_GEN`, `_EVENT_REGISTRY`, `_STATIONS`,
`_STATIONXMLS`, `_SOURCE_*`:

- A job-lane handler receives an immutable **`SessionSnapshot`** (pile + selection + view params,
  captured at dispatch) and returns a **`SessionDelta`** —
  `{traces_by_origin, events_by_origin, markers, scene}` — which the *generated dispatcher*
  applies in one synchronous, await-free `commit(expected_generation, delta)` after the
  handler's last await. Commit is the only mutation path.
- A shallow freeze is **not enough**: `_visible_traces()` hands out the actual committed `Trace`
  objects and `_pile()` deliberately shares them (`bridge.py:368-389`), and pyrocko operations
  mutate in place (`tr.lowpass(...)`). The snapshot is therefore a **`SnapshotReader`
  capability**: its accessors return cloned/chopped *work* traces and immutable scalar metadata,
  never raw committed `Trace` objects or the mutable pile — so an in-place operation on what a
  handler holds cannot reach committed state, and "a cancelled job leaves `_TRACES`
  byte-identical" is true by construction rather than by discipline.
- The transaction covers **all effects**, not just session data. Inventory: session data
  (traces/events/markers/scene), the GF-store registry and its files, staged inputs, module
  caches, view-lane caches. GF-store downloads (`bridge.py:3082-3096` writes the FS across a
  sequential await loop today) write under a job-unique temporary root and atomically
  rename+register at commit; cancellation removes the temporary root. Benign idempotent module
  caches (deferred imports) are explicitly outside the transaction; their interruption is safe
  because import machinery is never interrupted (the interrupt-gating rules in §6).
- The **destructive prologues are deleted** (`_replace_derived(origin, [])` before compute —
  `bridge.py:3125, 3558, 3854, 6011`): replacement happens inside the atomic commit. A failed or
  cancelled job therefore leaves the previous result intact — today a failed SeisBench rerun
  permanently destroys the previous annotations; under this contract that is unrepresentable.
- `commit` checks `expected_generation`: a job that ran against generation G while a mutator
  produced G+1 terminates as `Stale` — it never silently commits into a session it did not read.
- The view lane (`get_view`) stays sync, reads the committed generation only, and owns its own
  state explicitly: the pile/cutout memo caches, `_VIEW_NOTICES`/`_CUTOUT_NOTICES`, and the
  fix-scale-ranges memory `_OLD_DATA_RANGES` (`bridge.py:2150-2163`). No job-lane handler may
  read or write view-lane state; this scoping is what makes view interleaving safe despite
  `get_view` not being pure.

Python cannot make this fully static; the enforcement is structural: handlers receive only the
snapshot (no store reference), `commit` takes a plain value (not a coroutine), the store's
mutators are private, and a headless parity test asserts byte-identical `_TRACES` after a
cancelled job. That is the strongest available guarantee in this layer, and the review consensus
is that it is sufficient *because* the scheduler (below) controls where suspension can happen.

### 5. Two-lane concurrency: one view + one job, safe points, honest limits

Lanes are closed sets: **View = `GetView` and nothing else, ever. Job = every other Python
operation** (including ingest, loads, exports, warm-up). The ledger keeps ≤1 in flight per lane.
The interpreter remains the mutex; the JS event loop's run-to-completion is the synchronization
primitive; there are no locks.

Invariants at any point where a view may run (all eight hold or the design is unsound):

1. Committed state is one immutable snapshot with a generation.
2. The active job has not mutated anything reachable from that snapshot (guaranteed by §4:
   mutation exists only inside commit, and commit is atomic w.r.t. the event loop).
3. All job effects live in its private workspace or its returned delta.
4. The view reads exactly one committed generation and writes only view-lane-owned state.
5. `GetView` cannot change the session generation; anything that can is job-lane by metadata.
6. No second job, ingest commit, or reload runs at a safe point — only the view lane.
7. The view reply carries `(session_generation, view_sequence)`; Rust applies it only if both
   still match (generalizing today's `view_request_gen` guard, `mod.rs:3001-3021`, which round-one
   review attacked and could not break).
8. Cancellation can win only before the commit transition; after it, the answer is `TooLate`.

**Safe points are dispatcher-owned, not handler-improvised.** A chunkable job's inner loop calls
one primitive — `await op.progress(done, total, phase)` — which is *both* the yield point and the
progress heartbeat (a handler cannot gain interleaving without observability, or vice versa).
The dispatcher publishes the safe point only between chunks, i.e. only where invariants 1–6 hold
by construction. Awaits on external work (pyfetch, ONNX) are natural safe points because §4
forbids pre-await mutation. Implementation note: the yield must be a **macrotask** — the queued
`GetView` arrives as a worker `onmessage` macrotask, and an awaited already-resolved promise
drains only microtasks, while `asyncio.sleep(0)` rides `setTimeout` and its nesting clamp. Use a
MessageChannel-based zero-delay yield; add a regression test that a `GetView` completes during a
chunked job.

Liveness, stated precisely (no over-promise): a view request **already in the worker's message
queue** when the job yields runs before the job resumes; a view requested on the main thread *in
reaction to* a progress event may miss that particular safe point (the event needs a main-thread
round trip) and catches the next one. Fresh-view latency during a cooperative job is therefore
bounded by one chunk duration, not zero. A per-safe-point `Yielded/Resume` handshake would
guarantee servicing at every yield at the cost of a main-thread round trip per chunk — rejected
as not worth it; the one-chunk bound is the contract.

**What this promises, exactly**: the canvas always stays smooth (stale-render floor — already
true today); fresh wiggles arrive at every safe point of a running job (chunk boundaries and
external awaits); during an indivisible native kernel (NumPy/OpenMP/pyrocko C), fresh data waits
for the next safe point. No design on one interpreter can promise more, and the plan must not
claim preemption of native work. STA/LTA chunks naturally at its existing per-window chopper loop
(`bridge.py:3857`); SeisBench already suspends at inference. A job that never yields degrades to
stale-but-smooth plus watchdog-offered cancellation — never to a freeze.

**Rejected alternative, recorded**: a second Pyodide interpreter. The session is one object graph
(traces, provenance, responses, events, stations, caches); duplicating it means replicating every
mutation across heaps — a distributed-consistency problem inside one tab, plus a second
multi-second boot and doubled memory on wasm32. All three reviewers independently disqualified
it.

### 6. Cancellation

Ladder, cheapest first:

1. **Queued**: pure ledger bookkeeping — remove, `Done(Cancelled)`. Ships first.
2. **Cooperative**: a cancel flag checked inside `op.progress(...)`; the job raises, the
   dispatcher discards its workspace (nothing was committed — §4), replies with a typed
   `Cancelled` result.
3. **Interrupt**: the worker allocates the interrupt buffer at boot
   (`py.setInterruptBuffer(sab)`) and posts it main-ward; cancel of a non-cooperating running job
   writes `2` from the main thread — the only mechanism that works while the event loop is held.
   `KeyboardInterrupt` is a `BaseException`: the generated dispatcher today catches `Exception`
   only (`pybuild/snuffler_py_plugin.py:164`), so an interrupt would escape raw across the worker
   boundary as a stringly error. The generator must emit an explicit `except KeyboardInterrupt`
   arm returning the typed cancellation variant, and `run_snuffling`'s blanket `except Exception`
   (`bridge.py:6248`) must re-raise cancellation rather than converting it into a success-shaped
   result.
4. **Restart**: watchdog escalation (below). Never automatic for a job that is merely slow — a
   long scientific computation with a live heartbeat is healthy.

**Linearization** (cancel racing completion): an execution status word in a SharedArrayBuffer,
keyed by `(epoch, request_id)` exactly like the pending map (so slot reuse across jobs cannot
alias), CAS'd — the dispatcher moves `COMPUTING → COMMITTING` immediately before applying the
delta; the main thread moves `COMPUTING → CANCELLING` when cancelling. Whichever CAS wins
defines the outcome: `Cancelled` (nothing committed) or `TooLate` (result applied, reported as
such — the UI never claims a committed result was cancelled). One terminal event per job, ever.

Two gating rules make the interrupt itself safe (without them the ladder can tear the very
atomicity §4 exists for):

- **The interrupt byte is written only after (a) winning the `COMPUTING → CANCELLING` CAS and
  (b) an interpreter-owner word says the *target job* currently owns the interpreter.** A failed
  CAS resolves the cancel as `TooLate` (or by awaiting the in-flight terminal reply) with no
  interrupt write — a commit is Python bytecode and `KeyboardInterrupt` lands between bytecodes,
  so an ungated late escalation (e.g. the watchdog firing as the dispatcher enters commit) would
  interrupt the commit itself. If the *view lane* owns the interpreter at that moment, the
  cancellation is recorded pending and delivered when the job resumes.
- On losing its own CAS, the dispatcher clears the interrupt byte *before* running any discard or
  reply code, and the generated `except KeyboardInterrupt` arm encloses reply construction too —
  so a stray interrupt cannot detonate inside the reply path and break one-terminal-event.

Defense in depth for the residual stray-interrupt case: a `KeyboardInterrupt` that lands in the
view lane is retried silently, **but only after invalidating every view-owned cache and the
freeze memory** (`_PILE` memo, `_CUTOUT_VEC`/`_CUTOUT_TRACES`, notices, `_OLD_DATA_RANGES`) —
the cutout cache installs in two assignments (`bridge.py:1393-1395`), so an interrupt between
them leaves a torn key/value pair that a naive retry would serve as a cache hit of wrong data.

### 7. Worker recovery, epochs, replay

Everything the ORT client already does (`bridge_worker.ts:540-611`), applied to the Pyodide
worker, plus epoch fencing:

- One `recreateWorker(reason)` seam in `bridge.ts`: reject every pending entry exactly once,
  clear per-lane state, null and replace the worker, report `Restarting → Ready/Failed`. Handles
  `onerror`, **`messageerror`** (currently absent entirely), boot deadline, and watchdog
  escalation.
- `RuntimeEpoch` stamps every request, reply, progress event, and runtime message. A message
  whose epoch is not current is ignored; the pending map is keyed by `(epoch, request_id)`; each
  worker's listener captures its own instance. This closes the stale-reply/stale-event races that
  round one constructed against an epochless restart. **The first epoch has an owner**: the early
  worker is created by `index.html` before `bridge.ts` loads (`index.html` stores
  `{ worker, queue }`; `bridge.ts:99-160` adopts and replays) — the bootstrap mints epoch 0 and
  stores it alongside (`{ worker, queue, epoch }`), stamped onto boot messages, so no message
  ever exists outside epoch fencing.
- Three identities, never conflated: `RuntimeEpoch` (worker instance), **`SessionEpoch`** (bumped
  by every dataset-replacing commit), `StreamId` (one live SeedLink source). Ingest requests
  carry the `SessionEpoch` they were drained under; a dataset replacement atomically closes the
  live stream or bumps the session identity before any later callback can submit — today
  `load_url` does *not* close the live handle while `empty_scene` does (`mod.rs:1690-1708` vs
  `:1613-1629`), so a delayed live batch can append into a replacement dataset; the fence closes
  that by construction.
- On epoch death, **every** job terminates exactly once: `Dispatching`/`Running` as
  `Lost(epoch)`, still-queued ones as `Failed(RuntimeLost)` — queued intent is not silently
  replayed against a rebuilt session whose state may differ. Nothing is auto-retried: a mutation
  whose execution was ambiguous must not be replayed silently. Replay of recoverable *dataset
  sources* (example slug, URL, retained file handles) runs as new, visible ledger jobs.
- Watchdog: per running job, a soft threshold flips the UI to "still working (elapsed)"; missing
  heartbeats after a cancel request escalate interrupt → offer restart. Hard deadlines exist only
  for operations whose metadata declares one (network fetches, boot) — compute jobs are never
  auto-killed.
- Timeouts for the boot phase and network operations get real bounds; `send()` gains a pending
  cap as a backstop assertion (the ledger already guarantees ≤2 outstanding).

### 8. Error ownership

| Scope | What lands there | What clears it |
| --- | --- | --- |
| `JobState::Done(Failed)` | scientific/network/decode failure, cancellation timeout | dismissal, or a newer job of the same kind |
| view lane `last_error` | a GetView transport/remote failure | the next successful GetView (retry with bounded backoff, stale view kept on screen) |
| `RuntimeState::Failed` | boot failure, crash, protocol fault, heartbeat loss | successful recreation (new epoch) |
| browser-adapter errors | picker/read/save failures | attached to their flow job |

Nothing except a runtime fault gates submission; **no error anywhere stops the view lane from
refreshing** — this deletes today's worst freeze (`Status::Error` + the `Ready` gate,
`mod.rs:2946-2948, 3014`). A success clears only its own scope. Ingest/socket errors attach to
the live-session flow job and mark the stream degraded; they never brake the app.

### 9. Progress transport and the wire lifecycle

A generic `OperationEvent { job_id, epoch, event: Started | Progress { done, total, phase } |
Heartbeat }` variant in the existing `WorkerEvent` (`bridge.proto:74`), posted by the
dispatcher's `op.progress(...)` primitive and delivered into the ledger via the existing event
callback path (`bridge.ts:110-117`). **There is deliberately no `Finished` event: the RPC reply
is the single authoritative terminal channel** (two terminal sources would race), and transport
death synthesizes `Lost`. SeisBench migrates to it; the 100 ms polling loop and
`refresh_seisbench_progress`'s status-string writes (`mod.rs:442-468`) are deleted. STA/LTA
reports per-window counts from its chopper loop; downloads report bytes; figures report phases.
The transport auto-registers every dispatch with the ledger (start/finish are authoritative from
the transport layer, not from callsites), so a delayed handler always yields at least
indeterminate activity with elapsed time — a callsite cannot forget visibility.

The lifecycle outcomes must exist **on the wire** — today's `BridgeResult` oneof carries only an
error or a concrete success payload (`bridge.proto:90-107`), and `ViewMeta`/`ViewParams` carry
no fencing fields (`view.proto:149-164`), so `Stale`, `Cancelled`, and invariant 7 are currently
unrepresentable and an implementor would stall or invent an out-of-band channel. Required proto
changes, consumed by the generated client as a typed
`CallOutcome<T> = Succeeded(T) | Cancelled | Stale { expected, actual }`:

- `BridgeResult` gains oneof variants `cancelled` (empty) and `stale { uint64 expected; uint64
  actual; }`; the generated Rust client maps them to `Outcome::Cancelled`/`Outcome::Stale`
  instead of `Failed`.
- `ViewParams` gains `uint64 view_sequence`; `ViewMeta` gains `uint64 session_generation` and
  `uint64 view_sequence` — the pair invariant 7 compares. (`TooLate` needs no wire form: it is a
  main-side cancel disposition followed by the job's ordinary success reply.)

### 10. Live ingest state machine

Per-stream: `Opening → ResetInFlight(seq 0) → Streaming(next_seq) → Lagged{dropped} → Closed`,
with `StreamId` + monotonic `batch_seq` on `IngestMiniSeedRequest`.

- **Parse before reset**: Python stages and parses the payload first, then applies clear+insert
  atomically in commit — a malformed reset batch fails without destroying the old scene
  (today `_set_traces([])` precedes parsing, `bridge.py:4100-4107`).
- Batch 0 (reset) must be **acknowledged committed** before seq 1 dispatches; the reset flag is
  owned by the stream state machine, not by a pre-consumed closure flag (`mod.rs:1181`). Reset
  batches are excluded from coalescing and never dropped; an oversized reset closes the stream.
- While a job runs, ingress accumulates in **one bounded coalescing mailbox** (bytes + time
  bound), drained as a single job-lane commit **only when the job lane is idle** — never at a
  running job's safe points, which would violate safe-point invariant 6 (only the view lane runs
  there) and would supersede every long analysis over a live stream. The time bound is a maximum
  *residence* before explicit drop/`Lagged`, not a drain deadline. Overflow surfaces as an
  explicit `Lagged { dropped_bytes }` gap marker — never a silent drop, never unbounded memory.
  A coalesced commit acknowledges its range as `seq_first..=seq_last` plus any dropped ranges.
- The bound covers the **framing buffer too**: the record framer accumulates until a full frame
  arrives (`mod.rs:1177-1181`, `seedlink.rs:83-105`), and a SeedLink v4 header can advertise an
  arbitrary `u32` payload length — a peer that never completes an advertised multi-gigabyte
  frame must hit a framing-buffer cap that closes the stream with a visible error, or the
  mailbox bound is bypassed before admission.
- Stale-stream fencing: a batch from a closed/replaced stream (`StreamId` mismatch) or a stale
  `SessionEpoch` is discarded (§7). The per-batch `Status::Ready` writes disappear with `Status`
  itself.

### 11. MarkerStore

Ownership and identity:

- `MarkerId { slot: u32, slot_generation: u32 }` — stable across insertions, deletions, and
  re-runs; selection and inline-edit state key on it (fixes the confirmed index-keyed edit
  corruption, `marker_table.rs:15` + `session.rs:413-419`). "Select all" is representable as
  `AllExcept(set)` rather than touching every marker.
- `store_generation: u64` bumped on any mutation, invalidating derived caches.
- Indexes: a time/interval index for draw + hit-test range queries; per-track/NSLC index so
  `marker_row_spans` is a lookup, not a glob scan. The >2000 overload check becomes an O(log n)
  range count. Hit-testing stays **exact at any count** (the 2000 threshold is a render policy
  only) but returns a bounded candidate type: beyond `MAX_HIT_CANDIDATES` at one position it
  reports `Overloaded { count }` and the UI says so (regression: many markers at one timestamp).
- The table renders `show_rows` over a cached sort permutation keyed on
  `(store_generation, column, ascending)`; recomputed on change only.

Bounded ingestion, enforced at the producer:

- The cap lives in **Python, before the large response is built**: `SnufflingResult` gains
  `uint64 markers_dropped`, and Python truncates deterministically (by detection strength/time)
  at the budget. The main thread never sees an unbounded message; the three-copy chain stops
  mattering because its input is bounded.
- Rust side, the store's **only** public ingestion API forces the outcome to be surfaced:

```rust
pub struct MarkerBudget(NonZeroU32);          // private ctor, validated against a hard cap
#[must_use]
pub enum CommitOutcome {
    Committed { added: usize },
    Truncated { added: usize, dropped: u64, budget: MarkerBudget },
}
impl MarkerStore {
    pub fn commit(&mut self, staged: StagedMarkers) -> CommitOutcome { /* … */ }
    // no push/extend/truncate exist
}
```

  "Never silently truncate" is a type-and-proto fact: `Truncated` must be matched and rendered
  ("showing first 20,000 of 143,201 detections"), and the user may deliberately raise the budget
  up to the calibrated hard cap.
- Derived-origin replacement (a re-run's markers) is an atomic pointer-swap delta, aligned with
  the session-transaction model. The O(n²) active-flag clear and per-marker NSLC string clones in
  `apply_snuffling_result` (`session.rs:428-467`) are replaced by store operations; NSLC interning
  moves into the proto (a per-result `repeated Nslc table` + indexes) so serialization, decode,
  and heap all shrink — a follow-up once the bound exists.
- Export paths (`markers_to_file`, save/export flows) become `Lane::Local` ledger jobs with
  activity; serialization of a large store chunks across frames or moves off the interaction
  path.

### 12. Test-harness contract

`data-snuffler-ready` is load-bearing: it derives from `Bridge::view_is_settled()` which requires
`Status::Ready` (`a11y.rs:90-94` ← `mod.rs:687-694`), and `tests/parity/parity_browser.ts:62`
blocks on it. The commit that splits `Status` **must** redefine it in the same change — as "view
lane settled" (view applied, none in flight, none desired), independent of job-lane activity — or
all tier-3 parity hangs. Gate 2's frame-budget assertions need a `snuffler_frame_stats()` wasm
export beside the existing `snuffler_a11y_stats()` (`app.rs:284-310` computes the EMA already but
exports nothing).

Opening a configuration panel must not auto-run its analysis: today selecting a snuffling opens
the panel and immediately starts it (`snufflings/mod.rs:106-205` — MinMax, STA/LTA, Cake,
Seismosizer, SeisBench alike). "Open panel" and "Run" become distinct actions with their own gate.

## Migration order

Each step is independently shippable and leaves every intermediate state compiling and passing
tests. Behavior-preserving refactors and behavior changes never share a commit.

1. **Marker editor bounded rendering** (independent; unblocks everything animated): MarkerStore
   skeleton (epoch + time/track indexes over the existing `Vec<Marker>`), **`MarkerId` (slot +
   slot_generation) with selection and inline-edit state rekeyed onto it** (the confirmed
   index-keyed edit corruption of Finding 1 is fixed here, not in step 12), `show_rows`
   virtualization, cached sort permutation, indexed hit-testing with the bounded candidate type,
   O(1)-amortized overload count, stop calling `ensure_station_distances` per frame. Adding an
   animated global spinner before this would force the expensive table to repaint continuously.
2. **Eifel browser regression**: marker-editor DOM and widget counts proportional to visible
   rows; interactions within the frame budget (add `snuffler_frame_stats()`).
3. **Worker recovery + epochs** (moved early — it is independent, is the only fix for the
   permanent-spinner-plus-60fps-burn trace, and its reference implementation already exists in
   the ORT client): `recreateWorker` seam, `messageerror`, epoch stamping, `(epoch, request_id)`
   pending keys, reject-exactly-once, boot deadline, pending cap.
4. **Operation metadata + typed submission**: `activity.proto` option, generator validation
   (`SystemExit`), generated `Operation` enum + `ActivityMeta` tables (prost-reflect or
   plugin-emitted Rust), `Submission`/`JobHandle`/`JobState`, `#[must_use]`, compile-fail fixture
   proving an RPC without metadata does not build.
5. **Ledger replaces `Status`**: runtime/activity split, navbar as a fold, transport
   auto-registration, delete the six `in_flight` bools and every `Status` write, `FigureSlot` and
   friends become handle-carrying enums, silent `()` submission APIs become `Submission`,
   **redefine `data-snuffler-ready` in the same commit**. Fix the four coalescing guards
   (`Effect::Read` rule; view lease held to terminal event — closes the two-concurrent-GetViews
   defect). Interim rules that hold from here until later steps land: (a) **the ledger runs
   single-flight — one global Python permit, view requests queue behind the job lane — until
   step 9 flips two-lane admission** (dispatching a view into today's still-eagerly-mutating
   SeisBench would recreate the round-1 torn-state trace three steps early); (b) the SeisBench
   polling loop is rewired to write the running job's ledger `Progress` until step 7 deletes it —
   progress must not go dark for two steps; (c) SeedLink submissions use QUEUE with **at most one
   queued ingest job whose request payload absorbs newly drained records under a byte bound** (a
   degenerate one-slot form of step 11's mailbox — no throwaway work), overflow drops-oldest with
   a logged warning until step 11 surfaces `Lagged`; (d) local validation failures with no RPC to
   attach to (e.g. `open_example`'s `UnknownExample`, `mod.rs:1784-1787`) mint an
   immediately-`Done(Failed)` ledger entry.
6. **DOM-glue inversion + flow jobs**: pickers return manifests/`HandleId`s; Rust submits; one
   handle per flow with per-item progress; warm-up becomes a visible Background job; watch poll
   and exports (including the `progress_overlay` deletion) become ledger jobs.
7. **Generic progress events**: `OperationEvent` in `WorkerEvent`, migrate SeisBench off the
   polling loop, instrument STA/LTA (per-window counts), downloads, figures.
8. **Session transactions in Python**: `SessionStore`, `SessionSnapshot`, `SessionDelta`,
   dispatcher-applied commit, delete the destructive prologues, `Stale` on generation mismatch.
   Execution is still strictly one-at-a-time here — this step only changes where mutation lives,
   and is verifiable headlessly (`tests/parity/test_bridge_snufflings.py`: cancelled/failed job
   leaves `_TRACES` byte-identical).
9. **Two-lane enablement**: view-lane admission at dispatcher-owned safe points
   (MessageChannel macrotask yield), chunk STA/LTA and RMS through `op.progress(...)`, lane
   assertions in the worker. This is a **capability flag flipped only after step 8's transaction
   parity gates pass** (byte-identical `_TRACES` after a cancelled job) — dispatching a view
   into a job that still mutates eagerly is the torn-state trace of round one.
10. **Cancellation**: queued-cancel first (pure ledger); then the interrupt buffer, status-word
    CAS, `KeyboardInterrupt` handling in the generated dispatcher, `TooLate`, watchdog
    escalation.
11. **Live ingest state machine**: `StreamId`/`batch_seq`, parse-before-reset, reset
    acknowledgement, bounded coalescing mailbox, `Lagged` gap surfacing.
12. **Bounded marker ingestion**: producer-side `MarkerBudget` + `markers_dropped`,
    `CommitOutcome`, atomic origin replacement, then NSLC interning in the proto.
13. **Module split last**: split `bridge/mod.rs` along the now-established concepts
    (`activity.rs`, `runtime.rs`, `jobs.rs`, `marker_store.rs`). Splitting earlier would merely
    distribute the same flags across more files.

## Regression gates

Existing harness: `tests/a11y_browser_validate.ts` (per-pass budgets), `tests/parity/*` (oracle,
inventory, scenarios, headless snuffling tests, `_RELOAD_KEEP` guard). New gates:

- Opening the Eifel marker editor keeps accessibility and widget trees bounded by visible rows;
  scrolling and hover stay within the frame budget (`snuffler_frame_stats()`).
- Starting any operation — snuffling, figure, export, warm-up, watch tick, ingest — immediately
  produces a ledger activity; a compile-fail fixture proves an RPC without metadata cannot build;
  grep-level check that no UI path renders a spinner without a `JobHandle`.
- Repeated Run clicks: the second submission returns `Joined`/`Queued`, never a duplicate and
  never a silent drop (double-submit race: both clicks before any await — admission is
  synchronous, so the dedup insertion happens before any suspension).
- A `GetView` completes while a chunked job is mid-run (macrotask-yield test); during an
  un-chunked native kernel the canvas keeps painting stale data and never freezes.
- A transient `GetView` failure does not stop subsequent refreshes; the stale view stays on
  screen; a later success clears the error.
- Two scene-mutating requests cannot execute concurrently or commit out of order; a job whose
  snapshot generation was superseded terminates `Stale`, never commits.
- A cancelled or failed job leaves the previous derived result intact (headless: `_TRACES`
  byte-identical); cancel of a queued job resolves immediately; cancel racing completion yields
  exactly one of `Cancelled`/`TooLate` with one terminal event; the interrupt byte is never
  written without winning the CAS *and* holding the interpreter-owner check; a stray
  `KeyboardInterrupt` in the view lane invalidates all view-owned caches and then retries
  silently; a cancelled flow terminates `Stopped { committed, skipped }`, never `Cancelled`.
- `Outcome::Stale` is exercised only by fault injection (a test that forges a concurrent commit);
  it firing in any user flow is a scheduler bug and fails the suite.
- Worker death: every job terminates exactly once (dispatched → `Lost`, queued →
  `Failed(RuntimeLost)`), the ledger holds only terminal entries afterwards, the next submission
  finds a recreated runtime, no repaint loop persists, and a stale old-epoch reply/event —
  including one from the pre-`bridge.ts` early worker — is ignored.
- Two `RenderFigure` submissions for different figure kinds in one frame both complete; neither
  supersedes the other.
- SeedLink: a malformed reset batch fails without destroying the previous scene; seq 1 never
  applies before seq 0's commit acknowledgment; mailbox overflow surfaces a visible gap; an
  incomplete oversized frame hits the framing-buffer cap and closes the stream visibly; a
  delayed live batch never lands in a replacement dataset (`SessionEpoch` fence).
- Marker bounds: a result over budget commits `Truncated` and the UI says so; hit-testing at one
  pathological timestamp reports `Overloaded`; the marker-editor gates of step 2 stay green at
  the budget cap.
- Opening a snuffling panel does not start its analysis; "Run" does.
- `data-snuffler-ready` reflects view-lane settlement under the new model (tier-3 parity stays
  green through step 5).

## Completion criteria

Application code cannot guarantee against browser or operating-system failure, but this work is
complete when every application-controlled workload is bounded, observable through one ledger,
serialized-or-snapshot-isolated where it touches Python session state, cancellable or
interruptible where it can be, and recoverable when its worker fails. A scientific computation
may take a long time; it must never look like an ignored click, silently accept an unbounded
backlog, silently destroy a previous result, or force the browser main thread to process work
proportional to an unlimited result in one frame. During a long job the waveform canvas keeps
rendering, fresh views land at every safe point, and what the navbar shows is a fold over the
ledger — it cannot disagree with reality, because it has no state of its own.
