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
