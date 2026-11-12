# 6. One activity ledger owns all Python work

**Status:** accepted · **Date:** 2026-08-28

## Context

Every computation runs in one Pyodide interpreter in one worker, and the interpreter is the
only mutex there is.
The app used to let each feature dispatch its own calls, with activity reported through one
mutable status string.
That produced permanent spinners for silently rejected work, a hung worker with no recovery,
writes coalesced as if they were reads, snufflings that cleared their previous result before
computing the next, and a marker pipeline whose work grew with markers × tracks × channels on
every frame: opening the marker editor on the Eifel example (1,061 traces, 717 catalogue
markers) dropped the app from 60 to about 10 frames per second.

## Decision

**One owner.** `ActivityLedger` in `snuffler-web-core` admits, queues, deduplicates, cancels
and settles every unit of Python work, and is the only queue.
The worker stays a relay that asserts the protocol; it keeps no queue of its own.
A second queue in the worker was rejected: every failure trace found in review exploited the
seam between two sources of truth.

**Operation metadata is a total function, enforced at build time.**
Every RPC declares its `Activity` as a proto method option: lane, admission policy
(`QUEUE`, `DEDUP` or `LATEST_WINS`), session access, cancellation support, whether it
supersedes running work and whether it may update markers.
The proto build fails on an RPC without one, so a new operation cannot bypass the ledger.
`LATEST_WINS` is only for reads; a write is never dropped in favour of a newer one, and two
figure requests of different kinds are different work (`DEDUP`).

**Two closed lanes.**
The view lane runs `GetView` and nothing else; the job lane runs every other operation, one at
a time.
A long job yields at safe points (chunk boundaries and external awaits), where a queued view
request runs, so the canvas keeps updating during analysis but not inside an indivisible
native kernel.
A second Pyodide interpreter was rejected: the session is one object graph, and duplicating
it means replicating every mutation across heaps, plus a second multi-second boot.

**Jobs are transactions.**
A mutating job works on a cloned workspace and commits atomically; a failed or cancelled job
leaves the previous state in place.
Cancellation races commit through one compare-and-swap: the outcome is `Cancelled` (nothing
applied) or `TooLate` (the result was applied and is reported as such), never both.
The RPC reply is the only terminal signal; a dead worker settles its jobs as `Lost`.

**Product rules that follow from it:**

- Every accepted operation is visible while it runs; nothing is dropped silently.
- Opening a snuffling panel never starts its analysis; only its Run or Plot control does.
- Compute jobs are never killed for being slow; only operations that declare a deadline
  (network, boot) have one.
- Mutations are never retried automatically after the worker dies: ambiguous execution is
  not replayed, and queued intent is dropped. Views and visible figures can be requested again.
- View work enters the activity readout only after 0.5 s, so ordinary panning shows none.
- Marker results are bounded (20,000 by default, 100,000 at most) and the truncation is
  shown, never silent.

## Consequences

- Adding an RPC means choosing its `Activity`; the build refuses to guess.
- The cancellation protocol between Rust, the worker and Python (`web/execution_control.ts`)
  is the most delicate code in the app; its validation suite pins every transition.
- Both generators reject mutating `LATEST_WINS` operations. Host tests cover transactional
  rollback, defensive stale commits, terminal settlement, and SeedLink sequence fences.
  Browser tests exercise real Python interruption, partial-load cancellation, view recovery,
  explicit figure execution, and worker replacement.
- Not built yet: a per-track index for marker hit-testing and a shared compute budget between
  the rayon pool and OpenMP.

## References

- `crates/snuffler-web-core/src/activity/` (`ActivityLedger`, `Outcome`)
- `proto/snuffler_web_proto/activity.proto`, `crates/snuffler-web-proto/build/activity.rs`
- `crates/snuffler-web-core/src/markers/store.rs`, `web/execution_control.ts`, `web/snuffler_bridge/service.py`
- `tests/codegen_missing_activity.py`, `tests/execution_control_validate.ts`
- `tests/bridge_runtime_validate.ts`, `tests/activity_browser_validate.ts`
