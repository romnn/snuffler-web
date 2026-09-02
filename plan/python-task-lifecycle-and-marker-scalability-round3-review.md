# Round-3 ensemble review: verifying the deep-review repairs

## Verdict

**Do not merge yet — repair pass required, then one re-verification round.** The round-2 repairs
are real: the ensemble re-attacked all 11 prior findings and confirmed 8 fixed with mechanism-level
citations. The remaining work is two blockers (one a regression introduced by the repair pass
itself), four high findings, and a short tail — all with concrete interleavings or deterministic
reproductions, none requiring a redesign. The architecture verdict is unchanged: the ledger, lanes,
execution-control protocol, and transaction shape are the right hill; every repair below is local.

## Review target and ensemble

- Branch: `feat/python-task-lifecycle-and-marker-scalability`
- Frozen head: `0348428dfa454eb8c18e00b94844b79455254073` (clean tree)
- Merge base: `a7f1cf8d10f0e5470b4619fef2180d871e0c724f`
- Diff SHA-256: `a0a03e7dfb0575d69a138773f4ad25141cc4d7317b7323d36ad8ff19d82b657e`
- Ensemble: one GPT-5.6 Sol xhigh reviewer (codex, read-only sandbox; concurrency/protocol-drift
  lens), two Claude Opus 5 read-only reviewers (Rust-core lens; Python/codegen lens), with the
  orchestrator (Claude Fable 5) independently tracing the execution-control protocol, the ledger,
  the file-load flow, and every new claim against the live tree and pinned Pyodide sources before
  adjudication.
- All reviewers were read-only; the tree did not change during the review.

## Prior findings: re-verification result

| # | Prior finding | Status |
| --- | --- | --- |
| 1 | SIGINT into Pyodide promise plumbing | **RESIDUAL** → new finding B1 (narrower windows remain) |
| 2 | lint gate red / wasm bridge unlinted | CONFIRMED-FIXED (3/3 reviewers) |
| 3 | composite flows without truthful identity | CONFIRMED-FIXED, with two follow-ons (B2, M2) |
| 4 | marker cardinality unbounded | CONFIRMED-FIXED, with two follow-ons (M1, L1) |
| 5 | file adapters aggregate all bytes | CONFIRMED-FIXED for waveform flows; **RESIDUAL** for auxiliary imports (H2) |
| 6 | request/recovery liveness | **RESIDUAL/REGRESSION** → B2, H3, M3 |
| 7 | SeedLink byte/state holes | CONFIRMED-FIXED (2/2 reviewers who attacked it) |
| 8 | frontend effects before Python success | CONFIRMED-FIXED (3/3) |
| 9 | main-thread work before activity | CONFIRMED-FIXED, one new leak beside it (H4) |
| 10 | typed error boundary not total | CONFIRMED-FIXED at the arms; prologue residual folded into B1 |
| 11 | address-keyed NSLC interning | CONFIRMED-FIXED (3/3) |

## Findings (fused, deduplicated, ranked)

### B1 — blocker: the interrupt byte is armable outside the target job's protected frames
Corroboration: codex CONFIRMED; orchestrator independently verified both windows; the opus
Rust-lens "byte only set from ComputingWithInterpreter" claim was adjudicated wrong on the merits
(`setInterpreterOwnership` arms on the `CancellingWithoutInterpreter → CancellingWithInterpreter`
re-grant, `web/execution_control.ts:126-128`, and `acquireInterpreter` performs that transition
before any Python frame exists, `web/bridge_worker.ts:322`).

Windows: (a) cancel while a request is queued/dispatching → the worker's initial ownership grant
arms SIGINT before `bridge.call` runs, so the interrupt can be consumed in Pyodide
coroutine-scheduling plumbing (transport failure or stranded Future instead of the typed outcome);
(b) `call()`'s `KeyboardInterrupt`/`BaseException` arms return without the commit CAS
(`web/bridge.py:7079-7086`), so a late cancel can arm the byte during failure-reply marshalling with
no target frame (delivery into the concurrent view or `FutureDoneCallback`); (c) `call()`'s
prologue and the activity lookup run outside the protecting try (`web/bridge.py:7007-7025`), so a
cancellation landing there escapes as an untyped transport failure. A latent fourth item rides
along: `endExternalWait` computes interpreter ownership with a job-lane-only predicate
(`web/bridge_worker.ts:355`), inverted for a future cancellable view rpc.

Repair: initial ownership grants never arm; the worker short-circuits a request already cancelled
before its first frame with a typed cancelled envelope and converts a `KeyboardInterrupt` that
escapes `bridge.call` into the typed cancelled reply; every exceptional exit from `call()` seals
through the commit gate before returning; the prologue moves inside one unified protected region;
`endExternalWait` receives the lane.

### B2 — blocker: composite flows outlive runtime loss and cross worker epochs
Corroboration: independently discovered by codex and the opus Rust lens; orchestrator verified the
error arm (`bridge/web/mod.rs:610` treats transport rejection as a per-file failure and continues)
and the `Cancelling`-only stop predicate (`dispatch.rs:427-431`).

Failure: worker dies (or the user clicks Restart Python) mid-`OpenDirectory`; the ledger records
the flow `Lost`, but `REGISTERED_LANE_JOBS` is not cleared, the stop predicate does not treat a
terminal owner as stop, and transport errors do not abort the flow — so the remaining children load
into the replacement interpreter's fresh session under a retired job while every new job-lane
submission fails with "the activity ledger granted an occupied lane".

Repair: `note_runtime_lost` clears the lane registrations; the flow stop predicate treats a
terminal or absent owner as interrupted; `load_file_page` aborts (and cancels the browser-side
load) on transport-class errors instead of recording them per-file.

### H1 — high: the workspace clone breaks event-object identity (regression, deterministic)
Opus Python lens, CONFIRMED; orchestrator verified. `_clone_dataset_state` deep-copies
`derived_events`, `event_registry`, and `active_mt` with three independent memos
(`web/bridge.py:5039,5044,5045`) and aliases `active_event` shallowly, while
`_replace_derived_events` retires by `is` identity (`web/bridge.py:499`). From the second
Seismosizer/Cake run onward every superseded derived event is orphaned in the registry; phantom
events accumulate until LRU eviction discards real catalog events. Repair: one shared memo across
the four identity-linked fields.

### H2 — high: auxiliary imports bypass the paging bound
codex CONFIRMED; orchestrator verified. `pickedFilePage` (`web/bridge.ts:878-896`) reads every
selected marker/event/station/StationXML file concurrently into one unbounded aggregate. Repair:
apply the per-file byte bound with typed `BrowserReadFailure` entries, and surface page failures on
the Rust consumers.

### H3 — high: early worker-script failures are lost and persistent boot failures recreate forever
codex CONFIRMED; orchestrator verified (`index.html` placeholder registers only `onmessage`;
`bridge.ts` `onerror` recreates unconditionally). Repair: the placeholder records error events for
adoption; recreation is bounded, and exhausting the bound publishes a synthetic runtime-failure
event so the ledger reaches `RuntimeState::Failed`; an explicit user restart resets the bound.

### H4 — high: a second PNG export while one is rasterizing leaks a permanently running local job
Opus Rust lens, CONFIRMED. `handle_export_requests` has no in-flight guard for PNG (unlike SVG and
marker export), and `start_png_export` overwrites `export_rx`, dropping the handle that owned the
first job (`app.rs:1537-1548`, `:474-479`). Repeats exhaust ledger capacity. Repair: guard on
`export_rx.is_none()`; complete a displaced job before replacing the receiver.

### M1 — medium: the marker accumulator starves event and active markers
Opus Python lens, CONFIRMED. Retention key `(score, -t, -index)` scores event markers 0.0, so an
over-budget marker file loses every event while dangling `event_hash` references survive
(`web/bridge.py:6569-6579`). Repair: retention tiers `(active, event, score, …)` so events are only
displaced by events.

### M2 — medium: `Outcome::Stopped`/`TooLate` are write-only — a cancelled partial load is silent
Opus Rust lens, CONFIRMED. The status line surfaces only `Failed`; `stopped()` discards accumulated
per-file failures and sets no diagnostic. Repair: surface `Stopped { committed, skipped }` (and
`Lost`) in the status line and keep the failure list as a diagnostic.

### M3 — medium: a hung non-cancellable rpc has no recovery affordance
Opus Rust lens CONFIRMED, orchestrator found independently. The read deadline covers only
`GetView`/`ListExamples`, and the only Restart Python affordance is gated on `JobState::Cancelling`
— unreachable for `NotCancellable` operations. Repair: offer Restart Python for a non-cancellable
job running past a threshold (no automatic worker destruction).

### L1 — low: `_cake_run` bounds markers but not its text payload (`web/bridge.py:6166-6174`).
### L2 — low: the typed `ReloadError` family is dead code while per-file failures cross as joined
strings, erasing tracebacks (`bridge/error.rs:17-103`; `bridge/web/mod.rs:610,698`). Repair: carry
per-file failures typed end-to-end (or delete the family; wiring chosen — the traceback affordance
is the point of the type).

## Verified clean this round (do not re-open)

- The execution-control CAS core: cancel-vs-commit, double-cancel, cancel-after-commit, the
  `PublishingCancellationInterrupt` fence, and `Atomics.wait` thread placement (3/3 reviewers).
- External-wait coverage of every JS-crossing await (41 sites audited) and non-nesting.
- View handoff arm/disarm ordering, including jobs parked in external wait.
- Worker epoch/instance fencing against stale replies; request-key checks precede settlement.
- Ledger admission, dedup, supersession-after-admission, prune-safe terminality, capacity bounds.
- Marker producer bounds + NSLC compaction + the Rust store's sole capped commit path.
- SeedLink pre-copy/aggregate bounds, admission-fenced reset, gap sequencing, single retirement.
- Paged waveform loads (bounds, oversized rejection, cancellation, no orphaned iterators).
- Presentation resets deferred to the Python/ack commit boundary.
- Descriptor-driven codegen parity (Rust/Python validate the same fields; the missing-activity
  fixture trips the guard); no TS/Python/Rust state-machine drift found.
- The Python workspace-transaction trace clone (shared trace memo holds; only the event maps leak —
  H1).

## Not covered

No live-browser QA this round — source, pinned Pyodide/pyrocko, and test-suite review only. The
repair pass must re-run the full gate set including the browser regressions.
