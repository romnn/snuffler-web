// Observe the application's real protobuf requests without adding a product debug API.
import { fromBinary } from "@bufbuild/protobuf";
import type { Page } from "playwright";
import { BridgeResultSchema } from "../../web/proto/snuffler_web_proto/bridge_pb.js";
import { ViewParamsSchema } from "../../web/proto/snuffler_web_proto/view_pb.js";

interface Call {
  epoch: number;
  id: number;
  method: string;
  request: number[];
  result?: number[];
  error?: string;
}
declare global {
  interface Window {
    __parityCalls: Call[];
  }
}

export async function installTransportProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    window.__parityCalls = [];
    window.Worker = new Proxy(window.Worker, {
      construct(Target, args) {
        const worker = Reflect.construct(Target, args) as Worker;
        if (String(args[0]).includes("bridge_worker.js")) {
          const post = worker.postMessage;
          Object.defineProperty(worker, "postMessage", {
            value(message: Call & { request: Uint8Array }, transfer: Transferable[]) {
              if (
                typeof message.method === "string" &&
                ["/GetView", "/SetActiveEvent"].some((method) => message.method.endsWith(method))
              ) {
                if (message.method.endsWith("/GetView")) {
                  window.__parityCalls = window.__parityCalls.filter(
                    (call) => !call.method.endsWith("/GetView")
                  );
                }
                window.__parityCalls.push({
                  epoch: message.epoch,
                  id: message.id,
                  method: message.method,
                  request: Array.from(message.request),
                });
              }
              return Reflect.apply(post, worker, [message, transfer]);
            },
          });
          worker.addEventListener("message", (event) => {
            const message = event.data;
            const call = window.__parityCalls.find(
              (call) => call.epoch === message.epoch && call.id === message.id
            );
            if (call) {
              if (message.ok) call.result = Array.from(new Uint8Array(message.result));
              else call.error = String(message.error);
            }
          });
        }
        return worker;
      },
    });
  });
}

export async function transportState(page: Page): Promise<{
  window: [number, number];
  activeTime: number | null;
}> {
  await page.waitForFunction(() => {
    return ["/GetView", "/SetActiveEvent"].every((method) => {
      const last = window.__parityCalls.filter((call) => call.method.endsWith(method)).at(-1);
      return (
        last?.result !== undefined || last?.error !== undefined || (!last && method !== "/GetView")
      );
    });
  });
  const calls = await page.evaluate(() => window.__parityCalls);
  const failed = calls.find((call) => call.error);
  if (failed) throw new Error(`${failed.method} failed: ${failed.error}`);
  const viewCall = calls.filter((call) => call.method.endsWith("/GetView")).at(-1);
  if (!viewCall?.result) throw new Error("no completed view request");
  const viewResult = fromBinary(BridgeResultSchema, new Uint8Array(viewCall.result));
  if (viewResult.result.case !== "view") throw new Error("the latest view request failed");
  const view = fromBinary(ViewParamsSchema, new Uint8Array(viewCall.request));
  let activeTime: number | null = null;
  for (const call of calls) {
    if (!call.result || !call.method.endsWith("/SetActiveEvent")) continue;
    const result = fromBinary(BridgeResultSchema, new Uint8Array(call.result)).result;
    if (result.case !== "activeEvent") throw new Error("active-event update did not succeed");
    const active = result.value.events.find((event) => event.eventHash === result.value.activeHash);
    if (result.value.hasEvent && !active)
      throw new Error("active event missing from registry reply");
    activeTime = result.value.hasEvent ? active!.time : null;
  }
  return { window: [view.visibleTmin ?? view.tmin, view.visibleTmax ?? view.tmax], activeTime };
}
