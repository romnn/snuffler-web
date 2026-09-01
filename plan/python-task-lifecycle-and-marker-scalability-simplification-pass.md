# Simplification pass (round 5)

One focused pass over the whole PR with a different lens from rounds 1–4: not new
correctness bugs, but concept count, DRY, drift surface, and type safety — with behavior
unchanged. Cross-vendor as before: one GPT-5.6 Sol xhigh read-only reviewer plus the
orchestrator's own structural pass; every reviewer claim was re-verified against the live
tree before being implemented.

## Base

- Branch head at start: `b2bf5c5` (clean tree; all round-3/4 gates green)
- Changes: uncommitted working-tree edits (no commits requested)

## Implemented

### Deleted dead surface

| What | Why it was dead |
| --- | --- |
| `ReloadFailure` / `ReloadBatchResult` proto messages | No producer on any side; the only consumer was a `From` impl whose only caller was its own test. The live typed path is the Rust `ReloadError` family. The test now exercises `BridgeError::traceback` through directly-built `ReloadSourceError`s |
| `OperationHeartbeat` proto message + oneof arm | No producer; Rust only matched it to ignore it |
| `IngestMiniSeedRequest.dropped_seq_first/last/bytes` + mailbox seq fields | Rust populated them; Python never read them. The mailbox's local `dropped_bytes` stays — it feeds the Lagged notice and resume log |
| `FigureResult.width`/`height` | Python measured and sent them; Rust decodes the PNG's own dimensions |
| `GfStoreLabel` rpc + servicer method | No caller on any side; superseded by `MountGfStore`'s reply |
| `SpecMeta` in `pybuild` `BUFFER_RESPONSES` | Stale codegen entry — the message does not exist, and the Rust generator's list had already dropped it (the two hand-synced lists had in fact drifted) |
| `MountGfStoreRequest` + the JS `_gfStoreDirectoryHandles` map/counter | The "registered handle id" was fiction: JS inserted the picked handle into a map and immediately read it back for the same call, Python only checked the string was non-empty. The rpc now takes `Empty`; the handle rides `WorkerRequest.handle` directly |
| `GfStoreInfo` NamedTuple | Only `label` was ever consumed; `gf_store_label` now reads the config, registers the store, and returns the label |
| `submit_job_with_outcome` | An adapter whose only caller was `submit_job`; both now delegate to `submit_flow_with_outcome` |
| Four unused schema imports in `web/bridge.ts` | Leftovers from the paged-loader refactor |

### `SceneMeta` carries aggregates, not per-trace metadata

Every consumer of `SceneMeta.tracks` was an aggregate reduction: track count, min/max
`deltat`, and the latest trace `tmax` (which the bridge already computes `scene.tmax`
as). The message now carries `track_count` / `deltat_min` / `deltat_max` beside the
bounds; `TraceMeta` and `_trace_meta` are deleted. This drops O(traces) of serialized
nslc/label/nsamples per scene reply — paid on every load and every live-ingest packet —
for fields nothing consumed. Per-trace metadata continues to ride `ViewMeta`, which the
view request pages. Side effect: `SceneMeta` is now `Copy`, so the bridge accessor stopped
cloning. One behavioral corner deliberately changed for the better: an empty-track scene's
`scene_deltatmin()` is now 0.0 (the documented "no data" value) instead of `INFINITY`;
every consumer already treated both as "no data".

### One authority where two copies could drift

- `ensure_flow_live(inner, flow, method)` — the post-picker/mid-flow cancellation guard,
  previously pasted at six sites with a hand-built `CallError::Cancelled` each.
- `replace_scene(inner, scene)` — six hand-rolled `scene = …; scene_gen += 1; clear_view`
  sequences (already diverging: `+= 1` vs `wrapping_add`) collapse into the one helper
  `install_scene` also uses.
- `settle_diagnostic(inner, result)` — the "success clears the diagnostic slot, failure
  takes it and still fails the job" match, previously duplicated by the two exports.
- `submit_event_state_call` — `set_active_event` / `update_event` / `remove_events` had
  three identical bodies differing only in the rpc; they are one-liners now.
- `callPython` in `bridge_worker.ts` — the eight-positional-argument `bridge.call`
  invocation plus the escaped-cancellation backstop was spelled twice (round 4 already had
  to edit both when `enterProtectedFrame` was added); it is spelled once now.
- `isCancellingState` in `execution_control.ts` — `requestCancellation`'s
  already-cancelling arm and `cancellationRequested` carried the same four-state list
  twice; a future state could have joined one and not the other.
- `_marker_retention_rank` in `bridge.py` — the accumulator's quality tuple and
  `_truncate_marker_result`'s ranking key were hand-synced duplicates (round 4 caught
  exactly this drift: the truncate path was missing the event tier). Both bounds now share
  one rank function; the truncator's separate active-peel became the rank's top tier,
  which preserves the survival guarantee.

### Types made states unrepresentable

- `addCakeModel` moved onto the guarded flow pattern: `pickCakeModel` (JS) is picker-only,
  and the Rust flow re-checks its ledger identity between the unbounded dialog wait and
  the typed `AddCakeModel` rpc — closing the cake half of the round-3 stale-child
  residual. `file_page_files` (all-or-error, shared with `file_page_texts`) feeds it.
- The worker's `activeJobControl` + `activeJobBlocksView` parallel variables became one
  `activeJob: { control, blocksView } | null`, so "blocks views but no active job" cannot
  exist.
- `Inner.runtime_epoch` is `Option<RuntimeEpoch>` instead of `Option<u32>`, so the runtime
  fence can no longer be compared against an unrelated integer.
- Python's `SessionCommitResult {expected, actual, committed}` became
  `Optional[StaleSessionCommit]` — "committed with mismatched generations" is no longer a
  representable value.
- `BrowserFileLoadPage`, `BrowserSourceAck(List)`, and `IngestMiniSeedRequest` demoted
  from the crate's public re-export surface to `pub(crate)`: they are wire shapes of the
  wasm bridge implementation, not app-facing model types.

## Adjudicated and not taken

- **Collapsing `ExecutionState` to a boolean** (reviewer suggestion): the shared
  `isCancellingState` classifier removes the real drift risk; the enum's four-way answer
  is what the CAS actually learned, and the execution-control validation suite asserts it.
  Rewriting the verified protocol's entry-point contract buys one deleted enum for churn
  in the most safety-critical file.
- **Moving `mountGfStore` onto the Rust-flow pattern**: concurring with the reviewer —
  `FileSystemDirectoryHandle` cannot cross the protobuf boundary, so the split would need
  an opaque-token protocol worse than the bundling. With the handle map gone, the
  remaining stale-child window is the mount call itself.
- **Unifying the paged waveform loader with the aggregate text imports**: resumable
  partial-success vs one-shot all-or-error; a shared loader would accumulate mode flags.
- The remaining dispatch entry points (`submit_job`/`submit_flow`/
  `submit_flow_with_outcome`/`submit_local_future`) each encode a real distinction
  (needs-own-handle × custom-outcome × local lane).

## Validation

| Check | Result |
| --- | --- |
| `task format` | applied |
| `task check` | passed (Rust host + threaded wasm, buf generate, mypy, tsc, execution-control state machine) |
| `task lint:fc` | passed (35 combos + ruff; one new finding — `clone_on_copy` on the now-`Copy` `SceneMeta` — fixed by removing the clone) |
| `task test` | passed: 131/131 |
| `task test:parity:bridge` | passed: 43/43 |
| `task typos` | passed |

Not covered here: live browser QA. The GF-store mount and cake-model picker paths are
browser-only; worth one manual pass (or `task test:a11y:browser` / the seisbench browser
suite against a rebuilt artifact) before merge.
