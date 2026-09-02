# Review record — python-task-lifecycle-and-marker-scalability plan

Multi-model adversarial review of `plan/python-task-lifecycle-and-marker-scalability.md`,
2026-08-28. Two rounds; the plan was rewritten after round 1 and surgically amended after
round 2. This file records who found what, the adjudications, and what was verified clean, so a
later round does not re-litigate settled ground.

## Verdict

**Converged.** The revised plan's core mechanisms — single Rust-owned ledger, two-lane
cooperative concurrency with dispatcher-owned safe points, `SessionSnapshot`/`SessionDelta`
commit-only mutation, CAS-linearized cancellation, epoch-fenced restart, producer-bounded marker
ingestion — were attacked by both round-2 reviewers and held (after the gating fixes below).
Round-2 findings were a different category from round 1 (specification gaps and contradictions,
not design defects), and every one is closed in the current plan text. A third round would
re-audit mechanical edits.

## Process

- **Round 1** (against the original plan): three reviewers with distinct lenses, plus
  orchestrator source verification of every load-bearing claim.
  - gpt-5.6-sol (codex, read-only sandbox, xhigh): adversarial concurrency/lifecycle.
  - fable-5 (read-only subagent): global-maximum architecture + type-system design.
  - opus-5 (read-only subagent): source verification + completeness.
- **Fusion**: plan rewritten from scratch around the unanimous core (all three independently
  converged on single-owner queue + snapshot/atomic-commit + structural safe points).
- **Round 2** (against the rewritten plan): gpt-5.6-sol attacking the new mechanisms; fable-5
  hunting contradictions, over-engineering, and under-specification. Both re-verified every
  round-1 P0/P1 as closed or partially closed; the partials produced the round-2 deltas, all
  applied.

## Key round-1 findings (all reflected in the plan)

1. Dual queue (Rust ledger + `python_queue.ts`) rejected — single owner (unanimous; each
   reviewer constructed a distinct split-brain trace against the two-queue design).
2. Destructive snuffling prologues (`_replace_derived(origin, [])` before compute,
   `bridge.py:3125/3558/3854/6011`) make naive two-lane unsound and let a failed rerun destroy
   the previous result → `SessionSnapshot`/`SessionDelta`, commit-only mutation (unanimous).
3. Two-lane interleaving already happens unguarded today (SeisBench awaits mid-mutation-window);
   ten of eleven async handlers await before mutating (opus-5, verified).
4. Two concurrent `GetView`s possible today (`invalidate_view_request` clears the in-flight slot
   early, `mod.rs:1420-1425`) (opus-5; orchestrator-confirmed).
5. Per-RPC metadata gate misses worker commands, exports, and local main-thread work; a third
   activity UI (`progress_overlay`) exists (fable-5 + opus-5) → `Operation` union.
6. `KeyboardInterrupt` is a `BaseException` and escapes the generated dispatcher's
   `except Exception` (fable-5; mechanism verified in `snuffler_py_plugin.py:164`).
7. Epochless restart loses/misroutes stale replies and events; `Lost ≠ Cancelled`; never
   auto-replay ambiguous mutations (gpt-5.6-sol).
8. SeedLink reset is consumed before success and Python clears the pile before parsing
   (gpt-5.6-sol + opus-5) → stream state machine, parse-before-reset.
9. Coalescing guards: latest-wins applied to mutating RPCs; error paths wipe data or pin stale
   generations (opus-5) → `Effect::Read` rule.
10. `data-snuffler-ready` derives from `Status::Ready` and gates the parity harness; must be
    redefined in the same commit that splits `Status` (opus-5).

Adjudications of round-1 disagreements: metadata declared as proto method options (fable-5 +
gpt-5.6-sol) *with* opus-5's constraint recorded (prost_build cannot read extensions → prost-
reflect or plugin-emitted Rust) and opus-5's `Operation`-union coverage requirement; warm-up kept
as a visible background job (gpt-5.6-sol wanted it removed; removal would re-create the
first-filter stall it exists to fix); gpt-5.6-sol's full per-message lease handshake trimmed to
the SAB status word + epoch fencing (same linearization, fewer round trips). RPC count settled
at 31 (the plan was right; the round-1 brief's 32 was a miscount).

## Key round-2 findings (all applied)

1. Interrupt write must be gated on winning the `COMPUTING → CANCELLING` CAS **and** an
   interpreter-owner check; dispatcher clears the byte before reply code (fable-5 +
   gpt-5.6-sol independently; the one real atomicity hole in the rewrite).
2. §10 "drain at safe points" contradicted safe-point invariant 6; resolution: idle-only drain,
   and `Outcome::Stale` reclassified as a defensive fault (unreachable in normal operation)
   (both independently).
3. `Stale`/`Cancelled`/view fencing had **no wire representation** (`BridgeResult`, `ViewMeta`);
   `OperationEvent::Finished` deleted — the RPC reply is the sole terminal channel
   (gpt-5.6-sol).
4. Flow jobs are not atomic jobs: `FlowId` vs child `JobId`, `Stopped{committed, skipped}`
   (gpt-5.6-sol).
5. Shallow-frozen snapshot leaks mutable shared `Trace` objects; GF filesystem effects escape
   the delta → `SnapshotReader` capability + effect inventory + staged GF writes (gpt-5.6-sol).
6. View-lane interrupt can leave a torn two-assignment cutout cache → invalidate view caches
   before silent retry (gpt-5.6-sol).
7. Complete per-RPC policy table added; coalescing keys typed (`RenderFigure` = DEDUP by kind,
   never LATEST_WINS); `Lane` coherence (proto VIEW|JOB, Rust adds Local; `exclusive_both_lanes`
   for the reload) (fable-5 + gpt-5.6-sol).
8. Rejection visibility made ledger-owned (every submission mints an entry; `#[must_use]` alone
   is bypassable by an empty match arm) (gpt-5.6-sol).
9. Migration interims specified: single-flight until step 9; SeisBench progress bridged until
   step 7; one-slot SeedLink absorber until step 11; `MarkerId` moved into step 1 (fable-5).
10. Early-worker epoch minting in `index.html`; `RuntimeEpoch`/`SessionEpoch`/`StreamId`
    separated; `load_url` does not close the live stream today (gpt-5.6-sol).
11. Tile pipeline explicitly exempted from "the only queue" (already bounded/epoch-fenced/
    latest-wins); `MapData`/`RenderFigure` freshness trade under job-lane serialization stated
    as deliberate (both independently).
12. SeedLink framing buffer needs its own cap (v4 header can advertise an unbounded payload)
    (gpt-5.6-sol).

## Verified clean — do not re-open

- `ensure_view`'s latest-wins coalescing state machine (`desired`/`applied`/generation guard,
  `mod.rs:2942-3043`): attacked in both rounds, sound; keep as the view lane's Rust half. Its
  only defects were the `Err` global brake and the early lease clear, both addressed.
- `get_view` memoization interleaving (invariants 4/5): attacked twice, holds — provided
  view-lane state stays out of `SessionStore`, which the plan now mandates.
- Epoch fencing against stale-worker replies: holds once every message carries the epoch
  (including the early worker's, now specified).
- Double-submit before any await: closed by synchronous admission on the single main thread.
- The one-queue/one-owner shape and the two-lane lane split themselves: no reviewer found a
  break that did not reduce to one of the applied deltas.

## Not covered

- No live-browser QA of any change (source + design analysis only; the plan's regression gates
  define the browser checks to build).
- The "3.5 s SciPy warm-up" figure and the Eifel fps/DOM numbers are prior measurements,
  not re-measured here.
- Pyodide `setInterruptBuffer` presence was verified in the shipped runtime
  (`dist/py-mt/pyodide.asm.mjs`); its between-bytecode delivery semantics are from upstream
  documentation, not exercised in this repo yet (the plan's step-10 gates exercise them).

## Round 3 — implementation review (2026-08-30)

A cross-vendor adversarial pass over the completed implementation on
`feat/python-task-lifecycle-and-marker-scalability` (merge-base `a7f1cf8`), to make the branch
merge-ready: gpt-5.6-sol (codex, read-only, Lens A cross-boundary races), opus-5 (read-only
subagent, Lens B simplification/type-safety/tests), fable-5 (read-only subagent, Lens C
architecture/performance), fable-5 orchestrating with independent source verification of every
HIGH. Verdicts: NOT-READY / MERGE-READY-AFTER-FIXES / MERGE-READY-AFTER-FIXES. All confirmed
findings below are FIXED in the working tree.

### The blocker (all three attacked it)

The `ControlState` machine held against wrong-target interrupt delivery, torn commits,
`Atomics.wait` deadlock, and epoch death mid-cancel. Two real defects survived review and are
fixed:

- **`beginCommit` could not win from `ComputingSuspended`** (opus-5 and fable-5, independently):
  a job whose safe-point resume outruns the view's release legally executes while suspended; the
  single-source CAS then misreported a never-cancelled job as `Cancelled` and rolled back its
  committed delta. Fixed: two-source CAS (`web/execution_control.ts`), pinned by
  `tests/execution_control_validate.ts` (now a `task check:ts` gate).
- **A lost commit CAS left the interrupt byte armed** (codex; orchestrator traced the escape
  route through Pyodide FFI-entry checks in `bridgeResult`): a late `KeyboardInterrupt` could
  fire during reply marshalling and surface as a transport `Failed`. Fixed: `beginCommit` clears
  the byte on loss.

### Confirmed hardening findings (all fixed)

1. [codex] Transaction/view setup ran outside `call()`'s protecting `try`: a cancel interrupt
   during the workspace clone escaped as a stringly transport failure and could strand
   `_active_snapshot`. Setup now runs inside the try; the view branch gained its own
   `except KeyboardInterrupt`.
2. [3×, incl. orchestrator] The session-transaction clone economy: 3 whole-pile deep copies per
   mutating job and 1 per interleaved view, plus double view-cache destruction per interleave —
   on the exact smooth-wiggles path the two-lane design exists for, and on every live-ingest
   batch. Fixed: one deep clone per transaction (the workspace); shallow snapshot/delta capture;
   view caches swap with the projected state and survive interleaves; handler-held trace
   identity is preserved (pinned by a new parity test).
3. [codex] Shift+R recreated `_SESSION_STORE` at generation 0, permanently stale-fencing every
   later view. Fixed: the store rides `_RELOAD_KEEP` (parity-tested). The same class existed for
   worker restart: `python_session_generation` was never reset on runtime loss — fixed in
   `note_runtime_lost`.
4. [codex] Idle worker death was invisible: `sync_runtime` only ever moved toward Ready, so with
   no RPC in flight nothing called `runtime_lost` and the next submission dispatched into a
   booting empty runtime. Fixed: the bridge tracks the observed runtime epoch and reports a
   change exactly once, resetting per-interpreter fences (session generation, warm-up latch,
   live-ingest state).
5. [codex] A failed live-ingest batch closed the stream but still drained and dispatched the
   buffered successor batches into the session. Fixed: the mailbox is cleared on every terminal
   path and drained only while `Streaming`; the seq-mismatch close also clears it, and the lag
   notice fires once per episode.
6. [codex] GF-store URL mounts wrote into the final directory during download, so a failed or
   cancelled attempt left stale files a later attempt would silently include. Fixed: per-attempt
   staging + atomic rename, cleanup on any exception.
7. [opus] A worker `started` event resurrected a `Cancelling` job, disarming
   `mark_cancel_too_late` and recording `Succeeded` for a commit-race loser. Fixed:
   `mark_running` transitions only from `Dispatching` (unit-tested).
8. [opus] The supersede sweep ran for rejected/joined submissions (cancelling work the
   replacement failed to replace), and `LatestWins` superseded before admission checks. Fixed:
   supersession is now a property of admitted submissions only (unit-tested).
9. [fable] The figure slot's `Joined` short-circuit swallowed same-kind different-params renders
   → permanently stale spectrum after a pan. Fixed: deleted (the ledger's DEDUP already joins
   identical work); slot bookkeeping is handle-aware.
10. [fable] A cancel landing between a multi-RPC flow's children found no pending transport call
    and was silently dropped. Fixed: flows honor the ledger's `Cancelling` state at child
    boundaries (`active_job_cancelled`), in the file-load loop and the export flow.
11. [fable] Plan Finding 10's marker-control gate was unimplemented and `marker_output` had no
    consumer. Fixed: Delete-selected/Clear-all are disabled (with an explanation) while a
    marker-producing job is queued or running.
12. [opus] `drain_records` rescanned the whole framing buffer and memmoved per packet
    (quadratic per WebSocket message). Fixed: offset-0 fast path + one drain per message.
13. [opus] `is_terminal` answered false for pruned records, parking a dispatch future forever
    with no waker. Fixed: absence ⇒ terminal (unit-tested).
14. [opus+fable] Per-frame costs: the activity snapshot cloned every record every frame (fixed:
    ledger revision counter + shared Rc snapshot rebuilt only on change), `unresolved_failure`
    was O(n²) on the idle path (fixed: single pass), `MarkerStore::edit` clone-and-compared per
    visible row (fixed: `edit_reported` with explicit change flags), figure requests rebuilt the
    full selected-span list per frame (fixed: generation-keyed freshness check), plus
    `take_snuffling`, `apply_file_manifest` content moves, bounded PNG-export polling, and no
    per-frame `Rejected` mint while the runtime is down.

### Simplification (applied)

- `SnapshotReader` deleted (adjudicated against codex's keep-as-marker: the isolation invariant
  is enforced by the installed workspace clone; the class was an unrealized mechanism sketch —
  recorded as a deviation).
- Activity metadata trimmed to consumed fields per language (TS: lane; Python: lane+effect;
  Rust: the rest); `ACTIVITY_POLICY_EXCLUSIVE` deleted (behaviorally QUEUE, nothing implemented
  it); `COOPERATIVE`/`INTERRUPT` collapsed to `CANCELLABLE` (the distinction had no consumer;
  the cancellation ladder is uniform). Every field is still validated non-UNSPECIFIED by all
  three generators.
- `Outcome::Stopped` (never constructed) and `RuntimeState::Booting`'s unread payload deleted.
- `bridge/mod.rs` split: the wasm module now lives in `bridge/web/{mod,live,dispatch}.rs`
  (live SeedLink client and ingest; permit/submission/transport machinery); `bridge/mod.rs` is
  the ~680-line cross-platform surface.
- The generator's duplicated local-ops table hoisted to one const; generated `#[allow]` →
  `#[expect]`; production `operationHistory` ring deleted (the SeisBench harness drains
  `takeOperationEvents` itself); marker-budget `try_new`/error apparatus → `from_wire`;
  `MarkerStore::insert` no longer has a sentinel-id failure path; `id_at` deleted and `index_of`
  demoted to private; dedup keys are moved, not cloned, on completion.
- A `clippy.toml` disallowed-methods seam now guards `rayon::spawn`/`spawn_local` (plan
  Finding 6's drift scenario); the sanctioned rasterization call site carries a narrow
  `#[expect]`.

### Adjudications and kept-as-is

- **`SessionStore` stale-commit path kept** (opus proposed deletion as unreachable): the job
  lane serializes commits, but it is a typed guard on a one-way door; a why-comment now says so.
- **`JobHandle` collapse into `JobId` rejected**: churn at ~40 call sites for no enforcement
  gain; the handle is the plan's named concept. Taste.
- **Reachability of the `beginCommit`-from-suspended interleaving** was never settled against
  Pyodide's WebLoop scheduling (both reviewers marked it SUSPECTED); the fix makes the question
  moot, which is the correct resolution for a scheduling-dependent race.

### Verified clean in round 3 — do not re-open

Interrupt-byte targeting across lane handoff (the `InterruptingOwned` publication handshake);
no `Atomics.wait` on the main thread and no deadlock; `markExecutionDone` convergence;
one-terminal-per-job including epoch death (permits released, pending rejected exactly once);
epoch fencing at all four boundaries (worker request/reply, worker events, main-thread dispatch,
Rust event drain); bounded view retry (one Python retry + 0.5 s Rust backoff, cannot race a
commit); worker lane assertions throw before mutating state; SeedLink framing bounds and
parse-before-reset; marker-store algorithmic bounds and identity stability; generator hard-fail
on missing/UNSPECIFIED activity metadata; `data-snuffler-ready` from view-lane settlement.

### Not covered in round 3

Wasm binary-size/startup deltas (unmeasured); handler-by-handler audit of Python `effect: READ`
honesty; a Rust-side codegen-failure gate equivalent to `tests/codegen_missing_activity.py`
(build scripts are not unit-testable here); remaining LOW per-frame folds in `paint.rs` and the
marker toolbar's O(n) selected count (bounded by the 20k default budget; flagged for the day the
100k cap is exercised).

### Round-3 verification pass (fixes attacked, 2026-08-30)

After the fixes landed, gpt-5.6-sol and opus-5 re-attacked them with a sharpened brief (break the
fixes; no re-audit of mechanical edits). Verdicts: BROKEN (one new finding, below) /
FIXES-HOLD-WITH-FOLLOW-UPS. **Every round-3 fix held**: the two-source commit CAS linearizes
against cancellation from both computing states; the shallow-capture aliasing surface is closed
(traced field-by-field through `_clone_dataset_state`); the `_DATA_GEN` non-monotonicity has no
consumer outside the swapped cache tuple; the ledger revision counter was verified complete
across all sixteen `&mut self` methods; the module split is item-set-identical to the pre-split
`mod web` modulo the enumerated fixes; figure freshness provably implies request equality (no
skipped render, no submit under generation churn). New findings, all fixed:

- [gpt-5.6-sol, CONFIRMED] Runtime loss (and a live sequence fault) closed the ingest state but
  left the SeedLink socket installed and visibly active while every later packet was rejected.
  Fixed: live retirement is centralized (`retire_live_session_state` + a deferred socket drop —
  deferred because retirement can run inside the socket's own callbacks); runtime loss,
  `close_live_ingest`, and the sequence-fault path all retire the socket and bump the session
  epoch.
- [gpt-5.6-sol, CONFIRMED] An explicit restart reported the same runtime loss twice (once
  directly, once from each rejected in-flight future). Fixed: the job-error path reports only if
  the observed epoch still matches the one it captured.
- [both, independently] The view projection was not interrupt-safe in ways its comments claimed
  (a partial `enter_view` could stash job caches as committed-view caches; a partial `exit_view`
  could orphan the workspace). Both windows are closed — the cache stash is gated on a completed
  projection and the workspace reference survives until reinstalled — and the comments now state
  the real gate (`GetView` is `NOT_CANCELLABLE` and the ownership handoff clears the byte;
  the store self-heals, it does not promise atomicity).
- [opus-5, CONFIRMED] The execution-control test pinned the positives but not the damaging
  negatives. Fixed: resuming or suspending an uncancelled job now asserts no byte is armed and
  `cancellationRequested` stays false.
- [opus-5] Two split items were one visibility level wider than needed; narrowed.

Recorded, not fixed (pre-existing, outside this branch's diff): `_invalidate_interrupted_view`
clears the user's frozen scale ranges on the view-retry path; a figure whose submission is
rejected while the runtime restarts is cached as rendered until its request changes; Python
`effect: READ` honesty remains unaudited handler-by-handler.

**Convergence**: both verification reviewers attacked the central mechanisms and could not break
the round-3 fixes; their new findings were a different category (live-socket lifecycle on
runtime death, test negatives, doc claims), all now closed and re-gated. A further round would
re-audit mechanical edits.
