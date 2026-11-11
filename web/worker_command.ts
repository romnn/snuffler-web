// Re-fetch and reload the `snuffler_bridge` package in the worker without losing the
// loaded pile.
// This is snuffler's Shift+R "reload snufflings": the snuffling logic lives in Python.
export const WORKER_RELOAD_SNUFFLINGS = "reloadSnufflings" as const;
export type WorkerCommand = typeof WORKER_RELOAD_SNUFFLINGS;
