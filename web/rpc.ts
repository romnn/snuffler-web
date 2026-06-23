// The canonical gRPC method id (`/package.Service/Method`) for a BridgeService
// rpc, derived from the generated service descriptor. Typed — a wrong rpc name is
// a compile error — and drift-proof: the names come from the .proto via
// protoc-gen-es, never hand-written. The worker and the browser glue use this
// instead of re-spelling method-name strings.
import { BridgeService } from "./proto/snuffler/bridge/v1/bridge_pb.js";

export function rpc(name: keyof typeof BridgeService.method): string {
  return `/${BridgeService.typeName}/${BridgeService.method[name].name}`;
}
