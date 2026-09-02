# Round-3 repair progress

Companion to `python-task-lifecycle-and-marker-scalability-round3-review.md`. The review document
is immutable; this file records the repairs and their verification.

## Target

- Review base: `0348428dfa454eb8c18e00b94844b79455254073`
- Repairs: uncommitted working-tree changes on the same branch (no commits requested)

## Repairs

| Finding | Repair |
| --- | --- |
| B1 interrupt-arming windows | `grantDispatchOwnership` (initial grant never arms); the worker answers a request cancelled-while-queued with a typed cancelled envelope before dispatching, and converts a `KeyboardInterrupt` escaping `bridge.call` (gated on the control word) into the same envelope; every exceptional exit from `call()` seals through the commit gate before rollback; the activity lookup and both lane bodies moved inside one protected try; `endExternalWait` computes interpreter ownership per lane |
| B2 flows crossing epochs | `note_runtime_lost` clears the lane registrations; `current_job_flow_interrupted` treats a terminal or absent owner as stop; `load_file_page` continues only past `CallError::Remote` and aborts the flow (cancelling the browser-side load) on transport/protocol/stale errors |
| H1 event identity | One shared deepcopy memo across `derived_events`, `active_mt`, and `event_registry` in `_clone_dataset_state`; `active_event` remains a float-tuple alias |
| H2 aux import bounds | `pickedFilePage` applies the per-file byte bound with typed `BrowserReadFailure` entries; `file_page_texts` is all-or-error so a partial station/marker import cannot silently drop files |
| H3 boot-failure bound | The eager-worker placeholder records `error` events for adoption; worker recreation is bounded at three consecutive boot failures, then publishes a synthetic `runtimeFailure` event (reaching `RuntimeState::Failed`) and halts; `restartRuntime` lifts the halt; a Restart Python button appears in the Failed state |
| H4 PNG export leak | One-export-at-a-time guard on the screenshot request; a displaced `PngExport` receiver completes its ledger job before being replaced |
| M1 marker retention tiers | `_MarkerAccumulator` ranks by `(active, is_event, score, -t, -index)` so event and active markers are only displaced by their own tier |
| M2 silent partial outcomes | `unresolved_stop_notice` surfaces terminal `Stopped {committed, skipped}` and `Lost` in the status line; `stop_file_load` keeps accumulated per-file failures as a diagnostic |
| M3 wedged non-cancellable rpcs | Restart Python also offered for a non-cancellable job running past 30 s (user-invoked; no automatic worker destruction) |
| L1 cake text bound | Arrival-table rows ride the accumulator payload, so the text agrees with (and is bounded by) the retained picks, with an omitted-count line |
| L2 typed reload errors | `FileLoadProgress` carries `ReloadSourceError`s; page read failures and per-file pyrocko rejections cross as `BridgeError::Reload`, preserving tracebacks for the hover affordance |

## Decisions

- A request already cancelled when the worker would dispatch it never reaches Python: answering
  with the typed envelope from the worker removes the need to arm SIGINT before the first
  protected frame exists.
- Mid-frame arming paths (requestCancellation, view release, external-wait exit) are unchanged;
  only the initial dispatch grant becomes non-arming.
- Cancellation-won takes precedence over a concurrent failure at the exceptional exits, matching
  the existing error-envelope path.
- Composite flows treat only `CallError::Remote` as a per-file verdict; everything below the rpc
  layer aborts the flow, because after a runtime loss further children would commit into the
  replacement interpreter's fresh session.
- The auxiliary text imports stay one-shot aggregate rpcs with an all-or-error contract rather
  than gaining a paged protocol; their payloads are line-based text and the bound plus typed
  rejection restores the memory guarantee without changing commit semantics.
- The dead `ReloadFailure`/`ReloadBatchResult` proto messages (no producer on either side) are
  left for a follow-up schema cleanup; the Rust `ReloadError` family they shadow is now live.

## Validation log

| Check | Result |
| --- | --- |
| `task format` | applied (rust, python, js, proto) |
| `task lint:fc` | passed: full feature matrix (35 combos), boundary checks, threaded wasm, ruff; two new `too_many_lines` findings were fixed by extraction (`show_recovery_actions`, `stop_file_load`/`set_reload_diagnostic`/`cancel_browser_file_load`), not suppression |
| `task check` | passed: Rust host + threaded wasm, mypy, tsc, execution-control state machine (including the new `grantDispatchOwnership` checks) |
| `task test` | passed: 131/131 |
| `task test:parity:bridge` | passed: 43/43, including the new clone-identity and event-retention-tier regressions |
| `task typos` | passed |
| Round-4 cross-vendor verification | one GPT-5.6 Sol xhigh + one Claude Opus 5 read-only verifier attacked the repairs; both credited most and returned regressions in the two hardest ones (below); all confirmed findings repaired, gates re-run green |
| Post-round-4 `task lint:fc` / `task check` / `task test` / `task test:parity:bridge` / `task typos` | all passed (35 combos; 131/131; 43/43) |

## Round-4 findings and their repairs

| Finding (corroboration) | Repair |
| --- | --- |
| Cancellation could still arm SIGINT between the worker's short-circuit and `bridge.call`'s first protected frame (GPT, confirmed by orchestrator against pinned Pyodide) | Ownership is now granted only from inside the protected try: `acquireInterpreter` never touches the request's own control word, and Python's first protected bytecodes call the worker's `enterProtectedFrame`. No transition into `...WithInterpreter` exists before the target frame, so the coroutine-scheduling plumbing always runs with the byte clear |
| `seal_execution` was not idempotent: a commit failing after the one-shot gate CAS re-sealed, read false, and mislabeled the failure as a cancellation (Opus) | The seal result is memoized |
| A ghost flow parked on a browser-local await (picker, directory walk) across a runtime loss could adopt a replacement job's identity once the freed lane slot refilled (both verifiers, independently) | Flows receive their own `JobHandle` (`submit_flow`/`submit_flow_with_outcome`); `job_flow_interrupted` compares slot identity exactly; progress is recorded by handle; the four picker importers gained a post-picker guard |
| The multi-select station importers still aggregated an unbounded total (both) | `pickedFilePage` enforces a 32 MiB aggregate budget with typed rejections beside the per-file cap |
| The `Aborted` arm dropped per-file failures accumulated before the flow broke (Opus) | The abort joins the typed failure list; the diagnostic carries all of it |
| `restartRuntime` consumed one slot of the fresh boot-retry budget (GPT) | The intentional teardown is not counted as a boot failure |
| A completed-but-unpolled rasterization made the guard reject the next export for one frame (GPT) | The export poll runs before new requests are consumed |
| The `Stopped` notice presented the `has_more` sentinel as an exact count (GPT) | `skipped` is documented and displayed as a lower bound |
| `_truncate_marker_result` lacked the event tier (GPT; Opus verified unreachable today since every producer pre-caps) | The event tier was added to its ranking anyway |
| The clone-memo comment claimed `_ACTIVE_EVENT` aliases the registry (GPT) | Corrected: only `_ACTIVE_MT` carries object identity; the tuple needs no copy |

## Known residuals (documented, not repaired)

- `addCakeModel` and `mountGfStore` bundle their picker and rpc inside `web/bridge.ts`, so a
  runtime loss while one of those pickers is open can still deliver one stale child into the
  replacement session. Narrow (requires loss during the dialog) and benign against the fresh
  session; the proper fix splits the picker from the rpc so the Rust flow can guard between them.
  *(Resolved for `addCakeModel` in the round-5 simplification pass — see
  `python-task-lifecycle-and-marker-scalability-simplification-pass.md`; `mountGfStore` stays
  bundled by design, since a directory handle cannot cross the protobuf boundary.)*
- `proto` still declares `ReloadFailure`/`ReloadBatchResult` with no producer; the live typed path
  is the Rust `ReloadError` family. Schema cleanup deferred to its own change. *(Done in the
  round-5 simplification pass.)*

## New regressions guarding the repairs

- `tests/execution_control_validate.ts`: the dispatch grant of a cancelled request reaches
  `CancellingWithInterpreter` with the byte clear; the mid-frame re-grant contrast case arms; the
  ordinary grant/release cases leave and clear the byte.
- `tests/parity/test_bridge_snufflings.py::test_workspace_clone_preserves_event_identity_across_the_event_maps`
- `tests/parity/test_bridge_snufflings.py::test_marker_accumulator_retains_event_and_active_markers_under_pressure`
