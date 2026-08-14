// The one worker-only command that is NOT a BridgeService rpc: mounting a live
// FileSystemDirectoryHandle (a DOM object that can't ride a protobuf request) for
// a GF store. Shared by bridge.ts (the sender) and bridge_worker.ts (the handler)
// so the id lives in exactly one place; the worker mounts the directory, then
// relays the real gfStoreLabel rpc. Hand-written, not generated: it is a
// transport detail, not part of BridgeService.
export const WORKER_MOUNT_GF_STORE = "gfStoreMount" as const;
// Re-fetch and reload bridge.py in the worker WITHOUT losing the loaded pile
// (snuffler's Shift+R "reload snufflings" — the snuffling logic lives in bridge.py).
export const WORKER_RELOAD_SNUFFLINGS = "reloadSnufflings" as const;
// Execute the imports bootstrap.py deferred past the boot (scipy). Sent once the
// first scene is on screen, so the cost lands while the user is looking at
// waveforms rather than at their first filter.
export const WORKER_WARM_DEFERRED = "warmDeferred" as const;
export type WorkerCommand =
  | typeof WORKER_MOUNT_GF_STORE
  | typeof WORKER_RELOAD_SNUFFLINGS
  | typeof WORKER_WARM_DEFERRED;
