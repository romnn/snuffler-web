# Python task lifecycle and marker scalability progress

| Step | Status | Regression gates | Result |
| --- | --- | --- | --- |
| 1. Marker editor bounded rendering | done | Stable `MarkerId`; virtualized rows; cached sorting; bounded indexed hit-testing; no per-frame station-distance request; lint and relevant tests | Added generational `MarkerStore`, cached endpoint/start indexes, stable hover/edit IDs, `show_rows`, cached ordering, bounded exact hit candidates, and once-per-generation distance requests; `task lint` and 113 tests pass. |
| 2. Eifel browser regression | done | DOM/widget counts scale with visible rows; hover and scrolling meet the frame budget; `snuffler_frame_stats()` is exercised | Added frame-stat export and served-app Eifel gate; 717 markers produce 106 projected nodes, frame EMA remains below 40 ms, and `task test:a11y:browser` passes 29/29. |
| 3. Worker recovery and epochs | done | Epoch-fenced messages; stale epoch ignored; pending rejected exactly once; worker recreates after failure; boot deadline and pending cap | Added epoch 0 ownership to the early worker; fenced requests, replies, runtime reports, and protobuf worker events; keyed pending calls by epoch and request ID; centralized crash, decode-failure, boot-timeout, and explicit restart recovery; capped pending calls; and kept worker-module imports safe. `task lint`, 113 tests, and the 31-check served-browser suite pass, including restart and fresh-RPC gates. |
| 4. Operation metadata and typed submission | done | All RPCs have generated metadata; invalid metadata fails generation; typed submission and dedup behavior tests pass | Added mandatory protobuf activity options for all 33 RPCs; generated exhaustive Rust, TypeScript, and Python metadata; made the generated Rust client carry `Operation` at every transport call; converted warm-up and GF-store mounting to typed RPCs; retained only reload as an explicit worker control; added typed job/runtime/submission states and synchronous dedup/rejection tests; and added a compile-fail generator fixture. `task lint`, 115 tests, Python/TS checks, and the 31-check served-browser suite pass. |
| 5. Ledger replaces `Status` | done | One activity ledger; no scattered in-flight flags or status writes; readiness is view-lane settlement; parity remains green | Deleted `Status` and all six ad-hoc in-flight flags; introduced the bounded Rust-owned single-flight scheduler; made every public async bridge API return a must-use `Submission`; made rejected, joined, queued, running, failed, lost, and completed work ledger-owned; rendered navbar activity as a fold; moved SeisBench polling and deferred warm-up into job progress/activity; added bounded one-slot live-ingest coalescing; made transport calls reject when no ledger job is active; and redefined readiness solely by view settlement. `task lint`, 117 tests, 30 bridge-parity tests, all 3 tier-3 scenarios, and the 31-check served-browser suite pass. |
| 6. DOM inversion and flow jobs | done | Pickers return manifests; flows retain visible activity across child jobs; warm-up, watch, and exports use the ledger | Added protobuf browser file manifests and opaque handle IDs; made file/directory/metadata pickers and reload return data only; moved all Python submissions and source acknowledgements into Rust; kept one visible ledger handle across each multi-file flow with per-file count/phase progress; made watch, warm-up, browser saves, SVG/PNG exports, and rayon rasterization ledger-owned; bounded terminal history; and deleted the modal `progress_overlay`. `task lint`, 115 tests, TypeScript/Python checks, and the 31-check served-browser suite pass. |
| 7. Generic progress events | done | Generic protobuf progress; no SeisBench polling; STA/LTA, downloads, and figures report progress | Added epoch/job-fenced protobuf `OperationEvent` transport with Started, Progress, and Heartbeat forms; request-local Python progress reporters; ledger event draining; STA/LTA window counts; waveform download phases/bytes; figure phases; and generic SeisBench progress with no polling path. The focused served SeisBench gate passes on WebGPU and CPU fallback, and the normal lint/test/browser gates remain green. |
| 8. Python session transactions | done | Dispatcher-only commit; failures and cancellation preserve committed traces; stale generation is typed | Added `SessionStore`, deep-cloned `SessionSnapshot`, capability-style `SnapshotReader`, `SessionDelta`, synchronous generation-checked commit, rollback on error/BaseException, typed Cancelled/Stale wire outcomes, dataset/filter/response/GF-store state coverage, and view sequence/session-generation fencing. A headless fault-injection regression proves failed STA/LTA and RMS calls leave committed trace bytes and prior derived results unchanged; all 30 bridge-parity tests and the 31-check browser suite pass. |
| 9. Two-lane enablement | done | Transaction parity passes before enablement; one view and one job lane; a view completes during a chunked job | Split admission into one Job permit and one View permit; made transport registration lane-aware; kept local work independent; used request-local progress attribution; projected committed state while a view interleaves with a transaction workspace; added view/session reply fencing; and added scheduler plus served SeisBench regressions proving GetView completes before a suspended job. STA/LTA and RMS use the same progress primitive as a `MessageChannel` macrotask safe point. The focused WebGPU and CPU fallback passes remain green. |
| 10. Cancellation | done | Queued, cooperative, interrupt, and restart ladder; CAS/owner fencing; exactly one `Cancelled` or `TooLate` terminal result | Added immediate queued cancellation; a request-local shared execution state with `COMPUTING → CANCELLING` versus `COMPUTING → COMMITTING` CAS linearization; target-owner-gated Pyodide interrupts; cooperative progress cancellation; explicit generated `KeyboardInterrupt` handling; transaction rollback and typed cancelled/stale envelopes; ledger-side one-terminal and too-late handling; and navbar Cancel plus delayed restart escalation. Unit, 31 bridge-parity, 31/31 browser, and manual long-running PhaseNet cancellation gates pass with the Eifel scene preserved. |
| 11. Live ingest state machine | done | Parse-before-reset; reset acknowledgment; bounded framing/mailbox; visible lag; stream and session fencing | Added explicit `Opening`, `ResetInFlight`, `Streaming`, `Lagged`, and `Closed` states; monotonic `StreamId`, `SessionEpoch`, and batch ranges; protobuf commit acknowledgements; reset-before-seq-1 dispatch ordering; whole-batch byte/time-bounded coalescing with surfaced dropped ranges; stale callback fencing; source retirement for dataset replacements; an 8 MiB incomplete-frame/reset cap; and Python parse-before-reset. Malformed reset and oversized v4 frame regressions pass, alongside 119 unit tests, 32 bridge-parity tests, and the 31/31 browser suite. |
| 12. Bounded marker ingestion | done | Producer budget; typed truncation shown in UI; atomic origin replacement; bounded payload; NSLC interning | Added validated 20,000-marker default and 100,000 hard cap; deterministic producer-side strength/time ranking before serialization; `markers_dropped` and budget fields; a must-use `CommitOutcome`; staged atomic origin replacement; visible truncation text; and per-result NSLC tables with marker indexes. The marker store exposes single inserts and bounded staged commits rather than unbounded batch extension. 120 unit tests, 33 bridge-parity tests, focused PhaseNet WebGPU/CPU marker decoding, and the 31/31 Eifel browser gate pass. |
| 13. Module split | done | Established activity, runtime, jobs, and marker-store concepts live in focused modules; full lint/test/browser suite passes | Split the proven seams into `activity.rs`, `bridge/runtime.rs`, `bridge/jobs.rs`, and `marker_store.rs`; removed obsolete TypeScript load/RPC orchestration; made bespoke adapters carry their ledger job IDs; made figure spinners handle-backed; enforced cross-lane reload exclusion and superseding-job cancellation; added indeterminate elapsed-time feedback; and added defensive interrupted-view cache invalidation/retry. Final gates: `task lint`, `task check`, 121 unit tests, 34 bridge-parity tests, 59/59 tier-1 cases, 3/3 tier-3 scenarios, focused PhaseNet WebGPU/CPU, and 31/31 served-browser checks all pass. |

## Post-implementation hardening

The follow-up architecture pass kept the one-ledger/two-lane/session-transaction shape and removed
redundant lifecycle state: queue position/reason mirrors and per-record worker epochs no longer
duplicate the authoritative queue and transport fence. Worker `Started` events now drive the
`Dispatching` to `Running` transition, every JavaScript bridge call requires a nonzero ledger job
ID, malformed progress and cancellation-control failures surface as diagnostics, and the worker
asserts same-lane and reload/view overlap violations.

Cancellation state and interpreter ownership are packed into one request-local atomic state around
the required `COMPUTING -> CANCELLING` / `COMPUTING -> COMMITTING` CAS operations. An intermediate
`InterruptingOwned` state prevents the worker from handing ownership to a view before the main
thread has published the interrupt byte. This closes the race in which a job interrupt could be
written after ownership had passed to the view lane. View refresh activity has a lower-level 500 ms
feedback grace period; egui schedules a repaint at the threshold, so short wiggle rerenders stay
quiet while a genuinely slow refresh still becomes visible.

Follow-up gates: `task lint:fc`, `task check`, 124 unit tests, 34 bridge-parity tests, the focused
PhaseNet WebGPU and CPU-fallback browser passes, and 33/33 served canvas/accessibility checks pass.
A live Eifel PhaseNet cancellation returned to `Ready · 1061 traces` with no console error.

## Round-3 hardening (implementation review, 2026-08-30)

A second cross-vendor adversarial pass (recorded in the review file's "Round 3" section) drove a
merge-readiness sweep. Cancellation: the commit gate accepts both computing states (a safe-point
resume may legally outrun the view's release) and clears the interrupt byte on loss;
transaction/view setup moved inside `call()`'s protecting try. Sessions: one deep clone per
transaction with shallow snapshot/delta capture; view caches swap with the projected state, so
interleaved views stop paying a whole-pile copy plus double cache rebuild; `_SESSION_STORE`
survives Shift+R and runtime loss resets the Rust-side generation fence. Recovery: idle worker
death is now observed via epoch tracking in `sync_runtime`. Live ingest: a failed batch clears
the mailbox instead of dispatching its successors; the lag notice fires once per episode.
Ledger: started events cannot resurrect a Cancelling job; supersession happens only for admitted
submissions; pruned records answer terminal; flows honor cancellation at child boundaries;
figure renders queue instead of joining on params changes; the marker editor's destructive
buttons gate on marker-producing jobs. Performance: revision-keyed activity snapshots, single-pass
failure resolution, report-based marker edits, generation-keyed figure requests, cursor-based
SeedLink framing, and assorted clone removals. Simplification: `SnapshotReader`,
`Outcome::Stopped`, `ACTIVITY_POLICY_EXCLUSIVE`, the cooperative/interrupt cancel split, the
production operation-history ring, and the marker-budget error apparatus are gone; per-language
activity tables carry only consumed fields; the wasm bridge module split into
`bridge/web/{mod,live,dispatch}.rs`; a `clippy.toml` seam guards bare spawns. New gates: the
execution-control transition table runs under `task check:ts`, three ledger regressions cover
the new guards, and two parity tests pin reload generation and view-projection identity.

## Deviations

- `JobState::Queued` no longer stores a second queue position or an unused reason, and
  `Dispatching`/`Running` no longer copy the already-fenced runtime epoch. These were implementation
  sketch fields, not invariants; removing them leaves one source of truth for each fact.
- The request execution control packs lifecycle and interpreter ownership into one atomic state,
  with an `InterruptingOwned` publication handshake around the shared interrupt byte. The required
  cancellation/commit CAS remains authoritative while ownership handoff is linearizable.
- View-lane activity waits 500 ms before entering the navbar fold, per the follow-up product
  requirement. The ledger still tracks and schedules it immediately.
- `SnapshotReader` was deleted rather than implemented (round 3): no handler read through it,
  and the plan's isolation invariant — handlers can never reach committed `Trace` objects — is
  enforced by the installed workspace clone instead of a capability object.
- `Outcome::Stopped` was deleted (round 3): flows keep one ledger handle and terminate
  `Cancelled` at a child boundary, so a partial-progress terminal variant never had a producer.
- The activity metadata surface was narrowed (round 3): `ACTIVITY_POLICY_EXCLUSIVE` and the
  `COOPERATIVE`/`INTERRUPT` cancel split are gone from the .proto (nothing consumed either
  distinction; the cancellation ladder is uniform for every cancellable rpc), and each language's
  generated table carries only the fields it consumes, while all three generators still validate
  the full declaration.

## Discoveries

- The Eifel browser regression originally sent the `m` shortcut while a removed accessibility proxy retained focus; the gate now opens Markers through the projected menu and waits for the Eifel job-lane load explicitly.
- Three legacy browser adapters still performed worker calls themselves after DOM inversion. They were retained for browser-only picking/mounting/reload behavior, but now propagate the Rust ledger job ID and the unused general load orchestration was deleted.
- The worker's claimed per-lane busy assertions were assignments rather than assertions; a second
  same-lane request could overwrite cancellation ownership. The worker now rejects the overlap as
  a protocol fault.
- Interpreter ownership and cancellation lived in separate atomic words, leaving a cross-thread
  check/write race that could deliver a job interrupt to an interleaved view. They now share one
  request-local atomic state and publication handshake.
- Deleting the old progress overlay left its `threads` cfg attribute attached to the following
  `persist` module. The feature-matrix lint caught and removed the accidental configuration gate.
- The SeisBench browser harness issued raw transport calls while the full app independently used
  the same worker. It now stubs the egui module and owns the low-level bridge exclusively.
