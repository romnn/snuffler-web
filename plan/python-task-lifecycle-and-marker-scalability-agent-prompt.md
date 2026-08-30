# Implementation-agent prompt — Python task lifecycle & marker scalability

Paste everything below the line to an implementation agent working in
`/Users/roman/dev/snuffler-web`. The prompt is step-scoped: one migration step per
invocation is the intended grain, resuming from the progress document.

---

You are implementing the **Python task lifecycle, two-lane concurrency, and marker scalability**
design in snuffler-web (a Rust/egui + Pyodide browser port of pyrocko's snuffler). The design
replaces a singleton `Status` string, six scattered `in_flight` flags, an unqueued worker, and an
unbounded marker pipeline with: one Rust-owned activity ledger, a two-lane (view + job)
execution model over dispatcher-owned safe points, snapshot/atomic-commit session transactions in
Python, CAS-linearized cancellation with interrupt support, epoch-fenced worker recovery, and
producer-bounded marker ingestion.

## Ground rules

1. **Read `plan/python-task-lifecycle-and-marker-scalability.md` first, in full.** It is the
   normative design: every type, protocol rule, policy assignment, migration step, and regression
   gate you need is specified there, with `file:line` evidence. It is the product of a two-round
   multi-model adversarial review; the decisions in it are settled — **do not re-litigate them,
   and do not edit the plan file. It is read-only.**
2. **Track ALL progress in a companion document**: `plan/python-task-lifecycle-and-marker-
   scalability-progress.md`. Create it on your first invocation if it does not exist, with:
   - a status table — one row per migration step (1–13): step, status
     (`pending` / `in-progress` / `done` / `blocked`), the regression gates that prove it, and a
     one-line result note;
   - a **Deviations** section: any place reality forced a departure from the plan's letter, with
     the reason and what you did instead. A deviation from the plan's *invariants* (as opposed to
     its implementation sketches) is not yours to make — stop and ask the user;
   - a **Discoveries** section: pre-existing bugs or surprises found en route (do not fix
     out-of-scope findings; log them here).
   Update the progress document as part of finishing each step, never retroactively from memory.
3. `plan/python-task-lifecycle-and-marker-scalability-review.md` is the review record: consult
   its "Verified clean — do not re-open" list before "fixing" something the review already
   cleared, and its findings when you need the why behind a plan rule.
4. **One migration step at a time, in the plan's order.** Each step must leave the tree
   compiling, linting, and passing tests before you report it done. The order is load-bearing —
   in particular, step 9 (two-lane enablement) is a capability flag that must not flip until
   step 8's transaction-parity gates pass, and step 5 must redefine `data-snuffler-ready` in the
   same change that removes `Status`, or the tier-3 parity harness hangs.
5. **Validation goes through the `task` runner only** (see the repo's lint skill /
   `taskfile.yaml`): `task lint`, `task test`, the parity tiers, and the browser checks. Never
   substitute ad-hoc `cargo clippy` / `pytest` invocations. For in-browser verification the app
   renders to a canvas — use the repo's qa skill (`?a11y=pointer` + the DOM projection).
6. Follow the repo's `CLAUDE.md`: no decorative comments, `#[expect]` over `#[allow]` with
   narrow placement, typed errors via thiserror in library code, eyre in tests, and **no commits
   unless the user asks for them** — finish a step, update the progress document, report, and
   stop.
7. Behavior-preserving refactors and behavior changes never share a commit when the user does
   ask you to commit.

## The invariants you are building toward (memorize; the plan §Target elaborates)

- The `ActivityLedger` in Rust is the **only** queue. There is no `web/python_queue.ts` — do not
  create one. The worker stays a dumb relay plus per-lane busy assertions.
- Lanes are closed sets: View = `GetView` only; Job = every other Python operation. At most one
  in flight per lane; the JS event loop's run-to-completion is the only mutex.
- Job handlers mutate session state **only** by returning a `SessionDelta` that the generated
  dispatcher commits synchronously (await-free) — no destructive prologues, no mid-run global
  writes. `SnapshotReader` hands out cloned work traces, never committed `Trace` objects.
- The RPC reply is the **sole terminal channel** per job (no `Finished` event); `Stale`,
  `Cancelled`, and the view fencing pair exist on the wire (`BridgeResult` variants,
  `ViewParams.view_sequence`, `ViewMeta.session_generation`).
- The interrupt byte is written only after winning the `COMPUTING → CANCELLING` CAS **and** the
  interpreter-owner check; a view-lane `KeyboardInterrupt` invalidates all view-owned caches
  before its silent retry.
- Every message, reply, and event carries `RuntimeEpoch`; on epoch death, dispatched jobs
  terminate `Lost`, queued jobs `Failed(RuntimeLost)`, exactly once, nothing auto-retried.
- Every submission mints a ledger entry (rejections included); no UI spinner exists without a
  `JobHandle`; no RPC exists without `ActivitySpec` metadata (the generators must fail without
  it — the plan's §2 records the prost-extension pitfall and the two working routes).
- Marker results are bounded at the **producer** (`MarkerBudget`, `markers_dropped`), committed
  through `CommitOutcome` (`Truncated` must be rendered), keyed by generational `MarkerId`.

## Where to start

Read the progress document (if it exists) to find the current step; otherwise begin with
migration step 1 (marker editor bounded rendering — it is independent of everything else and
unblocks the animated activity UI). For each step: read the plan's step text and the sections it
references, read the cited source files, implement, run the step's regression gates plus
`task lint` and the relevant `task test` targets, update the progress document, report what was
done and what the gates showed, and stop.

If a step's gates cannot pass as specified, do not weaken the gate: mark the step `blocked` in
the progress document with the evidence and stop for the user.
