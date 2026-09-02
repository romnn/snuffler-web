# Deep review: Python task lifecycle and marker scalability implementation

## Verdict

**Do not ship the current branch.** The global architecture is the right hill: one Rust-owned
activity ledger, one job lane plus one view lane over a single interpreter, request-local
cancellation/commit control, Python workspace transactions, and epoch-fenced recovery are the right
concepts. The implementation is not merely over-complex; several boundary seams are incomplete or
typed with the wrong domain concept. Those gaps reintroduce the exact outcomes this work is intended
to prevent: an indefinitely held lane, an unbounded worker/main-thread workload, a misleading
terminal outcome, or frontend state changing before the Python mutation commits.

The central mechanisms that did hold should be preserved. This is a repair pass, not a reason to
replace the ledger or add another interpreter.

## Review target and ensemble

- Branch: `feat/python-task-lifecycle-and-marker-scalability`
- Frozen base: `a7f1cf8d10f0e5470b4619fef2180d871e0c724f`
- Frozen head: `de1723205bf6b2c5d6455eaa704d078ff390d7b8`
- Frozen binary diff SHA-256:
  `61c2a8714a2b40358cc04129eb38488015f1c7d4a1248042d9c4b5bf04beaa61`
- Round one: three GPT-5.6 Sol xhigh reviewers and one Claude fable xhigh reviewer through the
  personal `my-claude` account.
- Round two: two focused GPT-5.6 Sol xhigh adjudicators plus a third GPT-5.6 Sol xhigh reviewer
  dedicated to the cross-vendor interrupt disagreement.
- The orchestrator independently traced the live tree, pinned Pyodide and pyrocko sources, ran the
  repository gates, and sampled cancellation in the served browser app.

All external reviewers were read-only. The source tree stayed at the frozen head throughout the
review; this report is the only repository change made by the review.

## Domain purpose

Snuffler Web lets a seismologist keep navigating a waveform dataset while Python loads data or runs
scientific work. Every operation must have one truthful identity and one terminal result; long work
must remain visible and recoverable; cancellation must affect only its target; failed work must not
change the committed session; and marker or file cardinality must not determine whether the browser
remains interactive.

## Findings

### 1. Blocker: SIGINT can be delivered into Pyodide promise plumbing

`ComputingOwned` currently means that no view displaced the job, not that the target job's Python
stack is executing. The state remains owned while the coroutine is suspended in a JavaScript
promise (`web/bridge_worker.ts:275-277,1169-1179`). Cancelling then writes SIGINT because the state
is `ComputingOwned` (`web/execution_control.ts:34-47`; `web/bridge.ts:363-379`).

SeisBench is a concrete path: `web/bridge.py:3701` awaits `sbi.annotate`, which eventually awaits the
separate ORT worker at `web/seisbench_infer.py:297`. Its `_seisbench_progress` path
(`web/bridge.py:563-569`) does not consult the operation reporter, so cancellation while ORT is
pending relies on SIGINT.

When the promise resolves, pinned Pyodide attaches Python `Future.set_result` through
`wrap_promise` (`docker/pyodide-mt/.work/pyodide/src/core/jsproxy.c:2801-2842`). The JS fulfillment
callback re-enters Python through `create_promise_handles_result_helper`
(`src/core/pyproxy.c:1391-1411,1476-1486`). CPython can consume the pending signal at its Emscripten
resume/periodic checks before control has returned to the protected `bridge.call` frame. Two failure
forms follow:

- The promise helper's failure path prints and clears the exception, then destroys both one-shot
  resolver handles. The Python Future can remain pending forever, so `releaseInterpreter` never
  runs and the job lane remains held.
- If the Future resolves first, `WebLoop.run_handle` re-raises `KeyboardInterrupt` when no loop
  handler is configured (`src/py/pyodide/webloop.py:455-475`). That exception is outside
  `bridge.call`'s typed cancellation arms and can escape to the worker.

Signal-clock phase decides whether either path occurs. Twelve live SeisBench cancellation samples
all settled cleanly, so this is not deterministic on every cancellation; the pinned-source path was
independently confirmed by Claude and GPT. A rare permanent lane hold or session-destroying worker
restart is still a blocker.

Local repair: introduce an explicit external-await state. The interrupt byte must remain disarmed
through promise resolve/reject and Python Future settlement; on coroutine resumption, cancellation
becomes a typed cancellation before handler work resumes. Add AbortController/ORT cancellation for
prompt liveness where possible, but do not treat abort alone as sufficient because its rejection
uses the same resolver path. Raw SIGINT is valid only while the state proves that the target job's
Python frame is executing.

Structural constraint: a simple outer `setInterpreterOwner(false/true)` is insufficient. Nested
awaits are invisible to the worker, and a view release currently grants ownership unconditionally.
The external-await state must be part of the same request-local control protocol and respected by
view handoff.

### 2. Blocker: the required lint gate is red, while its new enforcement misses the wasm bridge

`task lint:fc` fails on the frozen branch:

1. `Operation` is imported only under `svg-raster` at
   `crates/snuffler-web-egui/src/app.rs:14-15`, but `unresolved_failure` uses it in every feature set
   at `app.rs:1713`.
2. The new global disallowed-method policy (`clippy.toml:6-9`) rejects the app bootstrap's
   `wasm_bindgen_futures::spawn_local` at `crates/snuffler-web-egui/src/main.rs:16`.

The policy also does not enforce its intended invariant. `task lint:fc` uses `--no-deps`
(`taskfile.yaml:402-411`), `snuffler-web-egui` is a primary wasm target, and
`snuffler-web-core` has no wasm cargo-fc target. The wasm-only bridge code is therefore compiled as
a dependency but not linted as a primary package. Its bare spawns, including the ledger composition
point at `bridge/web/dispatch.rs:124` and deferred live-socket spawns, do not exercise the policy.

Fix the import, narrowly exempt genuine bootstrap/composition points, and make
`snuffler-web-core` a primary wasm lint target. The guard should reject an intentionally forbidden
spawn in a fixture; merely making the current gate green does not prove enforcement.

### 3. High: composite flows do not own a truthful activity identity

`WatchPoll` is generated as `Lane::Local` (`crates/snuffler-web-proto/build.rs:131-138`) but is run
through `submit_job` and calls `apply_file_manifest` (`bridge/web/mod.rs:1850-1863`), whose children
are job-lane `LoadBytes` or `LoadBundle` calls (`bridge/web/mod.rs:484-550`). Transport attribution
does not receive an explicit owner; it searches the global active list for any handle with the
child operation's lane (`bridge/web/dispatch.rs:24-43`).

Consequences:

- With no unrelated job active, a changed watched file fails with “Python call has no registered
  activity job”.
- With a job active, the watch child can borrow that unrelated job ID and either trip the worker's
  second-job assertion (`web/bridge_worker.ts:267-278`) or inherit the wrong cancellation/progress
  identity.
- Local lanes have no active permit slot in `ActivityLedger::try_dispatch`, so repeated one-second
  watch polls are not serialized as the intended single flow.

Ordinary open/directory/reload flows reuse the atomic `LoadBytes` operation even if a child is a
bundle. Their label, cancellability, supersession, and `marker_output` metadata are therefore false.
The marker editor is not gated while a bundle-capable flow can commit markers.

Cancellation semantics are also inconsistent. `apply_file_manifest` commits each child
independently. A boundary cancellation after an earlier commit returns `Cancelled`
(`bridge/web/mod.rs:499-503`), despite `Outcome::Cancelled` promising cancellation won before any
commit (`activity.rs:148-158`). If cancellation wins inside the final child, the file loop treats it
as an ordinary per-file failure and can return `Ok` after an earlier success (`bridge/web/mod.rs:543-559`),
making the outer flow `Succeeded`. The normal UI currently hides this defect because the false
outer `LoadBytes` metadata is non-cancellable; fixing the metadata without fixing the outcome would
make it reachable immediately.

Replace lane-based ambient lookup with an explicit, scoped flow context. Add real
`OpenFilesFlow`, `ReloadFilesFlow`, and `WatchPollFlow` operation types that own the job lane and
carry truthful marker/cancel metadata. Restore a terminal flow outcome that records partial progress
(`Stopped { committed, skipped }` or an equivalent typed result); do not overload atomic
`Cancelled`.

### 4. High: marker cardinality is not bounded end to end

The response bound is post-hoc. STA/LTA appends every peak to a protobuf repeated field
(`web/bridge.py:3937-3943,3969-3974`), and SeisBench does the same. Only after the full producer
returns does `run_snuffling` call `_truncate_marker_result` (`bridge.py:6588-6607`). The truncator
sorts the entire result and copies the winners (`bridge.py:6430-6455`). A sensitive run can allocate
millions of protobuf objects and O(n log n) ranking state before the 20,000-marker wire response
exists.

The bound is bypassed or leaked through side channels as well:

- `LoadBundleRequest` has no marker budget (`proto/snuffler/bridge/v1/data.proto:325-337`), and
  bundle loading extends the result with every event marker without truncation
  (`web/bridge.py:5302-5329`). A large bundle reaches Rust and the main thread unbounded.
- `_truncate_marker_result` does not compact/remap `nslc_table`, so unique NSLC entries for dropped
  markers remain in the response (`bridge.py:6382-6406,6430-6455`).
- `_event_state` serializes every accumulated registry event plus a metric row
  (`bridge.py:4451-4479`). Repeated catalog/event loads can therefore grow a second unbounded
  response even when marker rows are truncated.
- `MarkerStore::commit` enforces no store-wide capacity (`marker_store.rs:284-301`). Originless
  marker files and catalog results accumulate, so repeated valid 20,000-marker operations exceed
  both the default and the declared 100,000 hard cap.
- Truncation ranks event markers by time because they have no incidence strength. The active
  largest-magnitude event selected at `bridge.py:4293-4297` can be omitted, leaving Python's active
  event invisible in the table/canvas.

Move the budget into each producer and retain at most K candidates while counting all detections.
Pass a budget through bundles, compact every referenced side table, and bound the event registry
projection. Make the Rust store's only commit constructor enforce a global capacity after accounting
for atomic origin replacement. Preserve manual markers and mandatory semantic rows such as the
active event, and surface every omission.

### 5. High: file and directory adapters aggregate all bytes before the first commit

`openFiles`, `openDirectory`, and `reloadFiles` read every `File.arrayBuffer()` into one vector and
serialize one aggregate manifest (`web/bridge.ts:547-631,640-700,707-748`). Rust then copies and
decodes the complete protobuf (`bridge/web/mod.rs:292-307`) before `apply_file_manifest` begins its
per-file loop.

A normal SDS/day-file tree can therefore retain all file contents, the aggregate protobuf, and the
Rust copy simultaneously. The visible outer activity does not keep the main thread responsive
during the final `toBinary`/boundary copy, and no file can commit until enumeration and aggregation
finish.

Keep browser handles behind opaque IDs and let the Rust-owned flow pull bounded pages under a total
byte budget. File count alone is not a sufficient bound; one oversized file needs streaming or an
explicit, typed rejection. Cancellation must stop future enumeration and reads.

### 6. High: request and recovery liveness are not closed

`web/bridge.ts:82-100,291-360` keeps pending requests without a per-request deadline. A promise
settles only on a worker reply or worker recreation. `ListExamples` performs an unbounded fetch
(`web/bridge.py:524-532,5336-5355`) but is non-cancellable
(`proto/snuffler/bridge/v1/bridge.proto:214-222`). A stalled request therefore holds the job lane and
spinner indefinitely, with no Restart button because the job never enters `Cancelling`.

`GetView` is also non-cancellable. While one is stuck, `ensure_view` continues scheduling 17 ms
repaints (`bridge/web/mod.rs:2063-2091`), sustaining the spinner/repaint loop forever.

Deterministic boot failure is logged and rethrown, but eager boot discards the rejection
(`web/bridge_worker.ts:1183-1188`). The main client waits five minutes, recreates the same failing
worker, and has no path that constructs `RuntimeState::Failed`.

Runtime loss has another leak above the ledger. `runtime_lost` correctly makes queued ledger jobs
terminal, causing their `DispatchPermit` to return before the work closure runs
(`bridge/web/dispatch.rs:124-134`). Slot cleanup exists only inside those skipped closures. Handles
such as `examples_job`, `station_dists_job`, `map_job`, figure `slot.job`, and a queued `view_job`
can keep pointing to terminal jobs (`bridge/mod.rs:98-107,122,148`; clear sites in
`bridge/web/mod.rs:910,1041-1043,1791,1829,2200`). Examples can then join a dead handle forever;
figures can show an eternal local spinner; a queued view can keep readiness false after recovery.

Use operation-aware deadlines/abort where safe. A timeout that merely rejects the main promise is
insufficient if the worker still owns its busy bit; recreate the worker or prove that the operation
was aborted and released. Report deterministic boot failure promptly and stop after a bounded retry
policy. On runtime loss, clear every derived job slot centrally or make every slot accessor validate
that its handle is nonterminal.

### 7. High: SeedLink still has state and byte-bound holes

The live state machine closes several earlier defects, but four reachable holes remain:

1. A WebSocket `ArrayBuffer` is copied to `Vec<u8>` and appended before any size check
   (`bridge/web/live.rs:167-176,263-270`). The framer bounds one advertised v4 packet but can collect
   an arbitrary number of complete packets into one output vector (`seedlink.rs:80-148`).
2. Only sequence zero is checked against the 8 MiB bound before direct dispatch
   (`bridge/web/live.rs:526-554`). A large later batch with no active ingest job bypasses the mailbox
   cap.
3. Sequence zero changes state to `ResetInFlight` before `submit_job`. A synchronous rejection
   installs no `ingest_job` and runs no cleanup (`live.rs:563-575,613-720`), leaving a live socket
   whose subsequent packets are discarded.
4. If an entire queued batch is evicted, `Lagged.next_seq` advances, but the reset acknowledgement
   overwrites it with `expected_last + 1`; the empty mailbox cannot carry the gap
   (`live.rs:434-523,647-683`). The next real batch is rejected by the stale sequence and the direct
   fault path sets `Closed` without retiring the socket (`live.rs:563-571`).

Bound payload length before copying, bound aggregate framed output and every direct batch, and make
admission part of the reset transition. Preserve the highest received/dropped sequence even when no
payload remains. Every terminal path should use one retirement function that closes the socket,
clears the mailbox/handle, bumps the epoch, and surfaces the reason.

### 8. High: frontend/session effects occur before Python success

Python transaction rollback is sound for Python-owned state, but several Rust/browser effects sit
outside its success boundary.

`Session::start_seedlink` clears annotations before validating/connecting
(`crates/snuffler-web-core/src/session.rs:159-163`). Once a socket object is constructed, the bridge
clears source handles, scene/view data, event context, and station distances before reset batch zero
commits (`bridge/web/live.rs:731-776`). An invalid port or asynchronous connection failure can erase
the current presentation while Python still owns the previous dataset.

`Session::close_all` likewise clears annotations first (`session.rs:166-181`), while
`Bridge::empty_scene` clears browser handles, live state, event data, and station data before the
`EmptyScene` RPC is admitted or succeeds (`bridge/web/mod.rs:781-813`). Rejection during restart
leaves Python's old pile alive but removes its annotations and reload provenance.

Stage these effects and install them only with the successful Python result/reset acknowledgement.
The frontend half and Python half need one observable commit boundary even though they live in
different languages.

### 9. Medium: allowed marker and export work still blocks the main thread before activity exists

The Eifel fixes are real, but the index is not used by all hot paths. Each visible waveform span
calls `draw_marker_snippets`, which scans the total store (`paint.rs:825-838,1135-1216`). The
overload decision counts only the visible-time range (`paint.rs:1306-1320`), so 20,000 dispersed
markers with fewer than 2,001 visible still cause approximately `total markers × visible spans`
visits per repaint. The ordinary overview layers also scan the full store twice
(`paint.rs:1409,1472`).

The marker table caches its permutation, but Phase and Channel sorting allocate owned keys inside
every comparison (`marker_table.rs:149-173`). This is a one-time spike rather than a per-frame sort,
but it is still synchronous at the 20,000-marker default.

Activity starts too late for explicit exports:

- `markers_to_file` sorts and clones the complete set (`marker_table.rs:527-582`) before
  `save_markers` submits work (`app.rs:679-706`). The declared `MarkersToFile` local operation is
  unused.
- SVG generation runs synchronously before `save_image_svg` creates an activity
  (`app.rs:1407-1423`).
- PNG builds the same SVG before `begin_local(Operation::ExportPng)`
  (`app.rs:397-415`); when the rayon pool is unavailable, rasterization then runs synchronously in
  the render loop (`app.rs:419-456`).

Expose an indexed overlapping-marker iterator and compute the visible candidate set once per frame.
Precompute textual sort keys once per rebuild. Mint local activity before work begins, then offload
or cooperatively chunk marker conversion and vector/raster generation. Showing a spinner before a
multi-hundred-millisecond synchronous call is not enough; the frame must return so the spinner can
paint.

### 10. Medium: the typed error boundary is not total

Generated dispatch converts `KeyboardInterrupt` and ordinary `Exception` to protobuf envelopes
(`web/proto/snuffler/bridge/v1/bridge_dispatch.py:99-318`). The outer transaction boundary catches
the remaining `BaseException` subclasses only to roll back and re-raise them
(`web/bridge.py:6885-6894`). `SystemExit`, `GeneratorExit`, or a direct `BaseException` can therefore
reject the JS call and become a transport error, contrary to the rule that Python exceptions never
cross the JS boundary.

After the dedicated cancellation arm, the outermost `call` boundary must serialize every remaining
`BaseException` to `BridgeResult.error`. This catch belongs outside snapshot setup, view projection,
commit, and reply shaping; changing only the generated `except Exception` is insufficient.

### 11. Medium: marker NSLC interning uses recyclable object addresses

`_MARKER_NSLC_CACHE` keys tables by `id(result)` (`web/bridge.py:6382-6406`) and removes an entry only
when `_truncate_marker_result` succeeds (`bridge.py:6430-6455`). A producer that interns a marker and
then fails leaves the entry behind. CPython may reuse that address for a later protobuf result; the
new marker then inherits indexes from the old result's table without adding the corresponding NSLC
row. Rust silently filters an out-of-range index at `session.rs:431-443`, potentially turning a
channel-scoped marker into an unscoped one.

Make the intern table a local producer-owned value, or guarantee cleanup in the outer producer
scope. Do not use object addresses as long-lived semantic keys.

## Verified clean

The review deliberately attacked, and did not break, these parts:

- `ActivityLedger` owns normal admission, bounded queue depth, dedup/latest-wins policy, one permit
  per Python lane, cross-lane reload exclusion, terminal idempotence, pruning, and runtime-loss
  settlement (`crates/snuffler-web-core/src/activity.rs`).
- The request-local cancellation/commit CAS and `InterruptingOwned` publication handshake prevent a
  target job's byte from being written into an active view. Finding 1 is a missing external-await
  state, not a failure of those atomic transitions.
- Python mutators run in a deep-cloned workspace; failed, stale, and normally-cancelled jobs roll
  back; successful commit is generation-checked and synchronous (`web/bridge.py:4865-5092,6804-6896`).
- Worker instance/epoch fencing rejects pending requests once, ignores stale replies/events, and
  does not replay ambiguous mutations (`web/bridge.ts:143-181,230-247`).
- Stable `MarkerId` generations, bounded endpoint hit candidates, virtualized marker-table rows,
  and the cached sort permutation are sound once ingestion/storage are actually bounded.
- The default track-height regression is fixed, and the view-lane feedback delay correctly reveals
  work only after 500 ms (`activity.rs:251-260`; `app.rs:1144-1151`).
- SeedLink's Python mutation remains parse-before-reset and acknowledgement-fenced; the findings are
  in admission, byte bounds, and terminal Rust/socket state.

## Validation evidence

| Command / exercise | Result |
| --- | --- |
| `task test` | 127/127 passed |
| `task check:ts` | passed; all execution-control checks passed |
| `task check:py` | passed; mypy clean |
| `task test:parity:bridge` | 36 passed; 12 third-party deprecation warnings |
| `task test:a11y:browser` | 33/33 passed; Eifel virtualization/default tracks/500 ms grace/recovery all green |
| `task check:threads` | passed; expected unstable `atomics` target-feature warnings |
| `task lint:fc` | **failed** on the two blocker diagnostics in Finding 2 |
| Live Playwright SeisBench cancellation | 12/12 samples settled on the same epoch; this does not exercise a forced signal-clock alignment |

The passing tests establish that the implementation is substantially improved; they do not cover
the cardinality, failure, and interleaving counterexamples above.

## Minimum regression matrix

1. **Interrupt delivery:** a real-worker RPC awaits a controlled JS deferred promise. Force the
   exported Emscripten signal clock to the delivery boundary, cancel while suspended, resolve the
   promise, and assert one typed `Cancelled`, no worker restart, lane release, zero interrupt byte,
   and a successful next RPC in the same epoch.
2. **Feature enforcement:** `task lint:fc`, non-`svg-raster` egui, core as a primary wasm target, one
   legitimate bootstrap exemption, and a fixture that proves an unregistered spawn fails.
3. **Flow identity:** changed-file WatchPoll alone and behind a job; mixed waveform/bundle file
   flows; exact owner ID on every child; deduplicated polls; marker-control gating; cancellation
   before a child, between children, inside the final child, and after final commit.
4. **Marker bounds:** far-over-budget STA/LTA/SeisBench/marker/event/bundle inputs with unique NSLCs;
   assert bounded temporary cardinality, compact side tables, bounded event projection/envelope,
   active-event retention, and a hard-capped Rust store across repeated originless commits.
5. **File paging:** many files plus one oversized file; assert first commit before enumeration ends,
   a measured peak byte bound, and cancellation that stops future reads.
6. **Transport liveness:** never-resolving ListExamples and GetView, deterministic boot failure, and
   runtime loss with queued examples/station/map/figure/view jobs. Every handle must settle and a
   fresh request must dispatch.
7. **SeedLink:** oversized pre-copy payload, aggregate complete frames, oversized non-reset batch,
   rejected reset admission, whole-mailbox eviction, direct sequence fault, stale callback, and
   socket retirement on every terminal path.
8. **Frontend commit:** invalid/failed SeedLink start and rejected/failed Close all preserve scene,
   annotations, event state, source handles, and the previous live socket until success.
9. **Main-thread budget:** exactly 20,000 total markers with at most 2,000 visible across several
   spans; pan/hover, Phase/Channel sort, marker save, SVG, and PNG must keep frame gaps under the
   chosen budget, with activity visible before preparation begins.
10. **Typed cleanup:** handlers raising `SystemExit`, `GeneratorExit`, and direct `BaseException`
    return protobuf errors; a marker producer that fails after interning leaves no cache entry.

## Repair order

1. Restore and strengthen the lint matrix so subsequent work is guarded.
2. Add the external-await cancellation state and deterministic real-worker regression before
   relying on SIGINT anywhere else.
3. Replace ambient lane lookup with explicit flow ownership, truthful flow operations, and partial
   terminal outcomes; clear terminal job slots on runtime loss.
4. Close transport/boot deadlines and recovery.
5. Make marker production, side tables, event projection, and cumulative store capacity bounded.
6. Page browser file content under a byte budget.
7. Repair SeedLink admission/bounds/retirement and move frontend effects to the commit boundary.
8. Move local preparation out of the render loop and start activity before it.
9. Close the total error boundary and remove address-keyed interning state.

Each step can be made independently compilable and testable. Do not combine these behavioral fixes
with another module split; the existing conceptual modules are already close to the right shape.
