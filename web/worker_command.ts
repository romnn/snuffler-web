// The one worker-only command that is NOT a BridgeService rpc: mounting a live
// FileSystemDirectoryHandle (a DOM object that can't ride a protobuf request) for
// a GF store. Shared by bridge.ts (the sender) and bridge_worker.ts (the handler)
// so the id lives in exactly one place; the worker mounts the directory, then
// relays the real gfStoreLabel rpc. Hand-written, not generated: it is a
// transport detail, not part of BridgeService.
export const WORKER_MOUNT_GF_STORE = "gfStoreMount" as const;
export type WorkerCommand = typeof WORKER_MOUNT_GF_STORE;
