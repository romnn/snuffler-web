// Re-fetch and reload bridge.py in the worker WITHOUT losing the loaded pile
// (snuffler's Shift+R "reload snufflings" — the snuffling logic lives in bridge.py).
export const WORKER_RELOAD_SNUFFLINGS = "reloadSnufflings" as const;
export type WorkerCommand = typeof WORKER_RELOAD_SNUFFLINGS;
