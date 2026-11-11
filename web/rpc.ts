// rpc("loadBytes") returns the canonical gRPC id
// "/snuffler_web_proto.BridgeService/LoadBytes".
// Typed end to end: the argument is a BridgeService localName and the result is a
// BridgeMethodId, so the bridge seam (and the worker) reject anything that is not a
// real rpc id at compile time.
// Callers may equivalently pass BridgeMethod.loadBytes or the literal id directly.
import {
  BridgeActivity,
  BridgeMethod,
  type ActivityMeta,
  type BridgeMethodId,
  type BridgeMethodName,
} from "./proto/snuffler_web_proto/bridge_methods.js";

export {
  BridgeActivity,
  BridgeMethod,
  type ActivityMeta,
  type BridgeMethodId,
  type BridgeMethodName,
};

export function rpc(name: BridgeMethodName): BridgeMethodId {
  return BridgeMethod[name];
}
