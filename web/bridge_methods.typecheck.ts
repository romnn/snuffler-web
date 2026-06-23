// Compile-time regression test (no runtime code; not bundled — bun only bundles
// the bridge entrypoints). `task check:ts` (tsc) fails if any @ts-expect-error
// below stops erroring, i.e. if the method-id type hole ever reopens.
import { call } from "./bridge.js";
import { BridgeMethod } from "./proto/snuffler/bridge/v1/bridge_methods.js";
import { rpc, type BridgeMethodId } from "./rpc.js";

// rpc() rejects a name that is not a BridgeService rpc.
// @ts-expect-error not a BridgeService rpc
rpc("badMethod");

// A real name yields a BridgeMethodId; the literal id and the const member all
// satisfy the same union (so callers may use whichever they prefer).
const viaRpc: BridgeMethodId = rpc("loadBytes");
const viaLiteral: BridgeMethodId = "/snuffler.bridge.v1.BridgeService/LoadBytes";
const viaConst: BridgeMethodId = BridgeMethod.loadBytes;

// A bogus literal id is not a member of the union.
// @ts-expect-error not a real method id
const bogus: BridgeMethodId = "/snuffler.bridge.v1.BridgeService/Nope";

// A value typed as plain string is too wide to be a method id.
declare const wide: string;
// @ts-expect-error string is wider than the id union
const widened: BridgeMethodId = wide;

// The exported transport seam itself must require a BridgeMethodId, not a widened
// string (guards against call()'s param being loosened back to `string`).
type CallMethodArg = Parameters<typeof call>[0];
const seamTakesId: CallMethodArg = BridgeMethod.loadBytes;
// @ts-expect-error the seam must not accept a plain string
const seamRejectsString: CallMethodArg = wide;

void viaRpc;
void viaLiteral;
void viaConst;
void bogus;
void widened;
void seamTakesId;
void seamRejectsString;
