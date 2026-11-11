// The Pyodide worker: it boots our own Pyodide 314 build (pthread + OpenMP + SIMD;
// py3.14 / emscripten 5.0.3) with every wheel from the locally served py-mt/, and
// relays each bridge request to the Python bridge package (web/snuffler_bridge).
// This is the only runtime; the page is always cross-origin isolated (see
// index.html), which SharedArrayBuffer and wasm threads require.
//
// The modules around it:
// - pyodide_prefetch.ts downloads the runtime and the wheels ahead of Pyodide.
// - worker_events.ts posts boot and operation progress to the page.
// - ort_client.ts runs SeisBench inference for Python on onnxruntime-web.
// - execution_control.ts is the cancellation state machine shared with the page.
import { create, toBinary } from "@bufbuild/protobuf";
import {
  BridgeResultSchema,
  CancelledSchema,
  MlDevice,
  StringValueSchema,
} from "./proto/snuffler_web_proto/bridge_pb.js";
import {
  beginCommit,
  cancellationRequested,
  executionControl,
  markExecutionDone,
  setExternalWait,
  setInterpreterOwnership,
} from "./execution_control.js";
import { mlDevice, snufflerOrtInfer } from "./ort_client.js";
import {
  clearPrefetched,
  fetchOk,
  prefetchedByteLength,
  resolveUrl,
  startPyodidePrefetch,
} from "./pyodide_prefetch.js";
import { BridgeActivity, BridgeMethod, type ActivityMeta } from "./rpc.js";
import { WORKER_RELOAD_SNUFFLINGS } from "./worker_command.js";
import {
  BOOT_SPAN,
  RUNTIME_EPOCH,
  clearSeisbenchJob,
  postCountProgress,
  postMlDevice,
  postOperationStarted,
  postRuntimeFailure,
  postRuntimeInfo,
  postSeisbenchProgress,
  progressFraction,
  reportBootProgress,
  setBootPhase,
  setSeisbenchJob,
} from "./worker_events.js";
import {
  type WorkerFailureMessage,
  type WorkerReplyPayload,
  type WorkerRequest,
  type WorkerSuccessMessage,
} from "./worker_protocol.js";

const PYODIDE_BASE = new URL("py-mt/", self.location.href).href;

// TextDecoder.decode() throws "The provided ArrayBufferView value must not be
// shared" on a view backed by a SharedArrayBuffer in some engines.
// The threaded Pyodide heap is one, and Pyodide's Python->JS string conversion
// decodes straight off it (loadPackage()'s unpack_buffer, any Python string or
// exception crossing into JS).
// Most decodes do not trip this, so the native decode runs first and only the
// rejected inputs are copied and retried.
// Must run before Pyodide is loaded.
if (typeof SharedArrayBuffer !== "undefined") {
  const prototype = TextDecoder.prototype;
  const nativeDecode = prototype.decode;
  prototype.decode = function (input, options) {
    try {
      return nativeDecode.call(this, input, options);
    } catch (e) {
      const isShared =
        input instanceof SharedArrayBuffer ||
        (ArrayBuffer.isView(input) && input.buffer instanceof SharedArrayBuffer);
      if (!isShared || !(e instanceof TypeError)) throw e;
      const bytes =
        input instanceof SharedArrayBuffer
          ? new Uint8Array(input)
          : new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
      const copy = new Uint8Array(bytes.byteLength);
      copy.set(bytes);
      return nativeDecode.call(this, copy, options);
    }
  };
}

// Pyodide handles: the runtime, and the imported `snuffler_bridge.service` module
// whose `call` takes every rpc.
// Typed `any` because they are dynamic JsProxies with no static types.
let pyodideReadyPromise: Promise<any> | null = null;
let pyodide: any = null;
let bridgeService: any = null;
// Pyodide's interrupt buffer, shared with the page; installed by the first request.
let pyodideInterrupt: Uint8Array | null = null;
let mountedGfStoreCount = 0;

// The requests inside Python right now: at most one job and one view.
// A view may borrow the interpreter from a job suspended at a safe point, unless
// that job blocks views.
// One value for the job, not parallel variables: "a job is active" and "that job
// excludes views" are facts about the same request, and coupling them makes the
// state "blocks views, but no active job" unrepresentable.
let activeJob: { control: Int32Array; blocksView: boolean } | null = null;
let activeViewControl: Int32Array | null = null;
type PythonLane = ActivityMeta["lane"];

// The JS functions Python calls by name through the `js` module.
// - __snufflerOrtInfer: SeisBench inference (web/seisbench_infer.py).
// - __snufflerYieldToMacrotask: yields between the steps of a long Python loop, so
//   a cancellation can land and the worker can answer other messages
//   (snuffler_bridge/operation.py).
// - __snufflerSeisbenchProgress: SeisBench progress (snuffler_bridge/seisbench.py).
(globalThis as any).__snufflerOrtInfer = snufflerOrtInfer;
(globalThis as any).__snufflerYieldToMacrotask = (() => {
  // A MessageChannel message is a macrotask without setTimeout's minimum delay.
  const channel = new MessageChannel();
  const pending: Array<() => void> = [];
  channel.port1.onmessage = () => pending.shift()?.();
  return () =>
    new Promise<void>((resolve) => {
      pending.push(resolve);
      channel.port2.postMessage(0);
    });
})();
(globalThis as any).__snufflerSeisbenchProgress = (fraction: number, label: string) => {
  postSeisbenchProgress(fraction, label);
};

// Claims the interpreter for a request, on the lane its method belongs to.
// Overlaps the page should never send are protocol faults.
function acquireInterpreter(
  message: WorkerRequest,
  control: Int32Array,
  interrupt: Uint8Array
): PythonLane {
  const isReload = message.method === WORKER_RELOAD_SNUFFLINGS;
  const activity = isReload ? null : BridgeActivity[message.method];
  if (!isReload && !activity) {
    throw new Error(`protocol fault: unknown Python method ${String(message.method)}`);
  }
  const lane = activity?.lane ?? "JOB";

  if (lane === "VIEW") {
    if (activeViewControl) {
      throw new Error("protocol fault: a second view request reached the Python worker");
    }
    if (activeJob?.blocksView) {
      throw new Error("protocol fault: a view request overlapped an exclusive Python job");
    }
    activeViewControl = control;
    if (activeJob) {
      // The job keeps its cancellation state while the view owns the interpreter.
      // Its interrupt is re-armed when the view releases the interpreter.
      setInterpreterOwnership(activeJob.control, false, interrupt);
    }
  } else {
    if (activeJob) {
      throw new Error("protocol fault: a second job request reached the Python worker");
    }
    // A reload replaces the modules a view would run in.
    const blocksView = isReload;
    if (blocksView && activeViewControl) {
      throw new Error("protocol fault: an exclusive Python job overlapped a view request");
    }
    activeJob = { control, blocksView };
  }

  // Interpreter ownership is not granted here.
  // The control word stays ...WithoutInterpreter, where cancellation cannot arm
  // SIGINT, until Python itself calls enterProtectedFrame from service.call's
  // first protected bytecodes.
  // So no interrupt can be consumed in the coroutine-scheduling plumbing between
  // this point and the target frame.
  return lane;
}

// Ends a request and, when a view finishes, hands the interpreter back to the job
// it displaced.
function releaseInterpreter(lane: PythonLane, control: Int32Array, interrupt: Uint8Array): void {
  let jobWaitingForInterpreter: Int32Array | null = null;
  if (lane === "VIEW" && activeViewControl === control) {
    activeViewControl = null;
    jobWaitingForInterpreter = activeJob?.control ?? null;
  } else if (lane === "JOB" && activeJob?.control === control) {
    activeJob = null;
  }
  markExecutionDone(control, interrupt);
  if (jobWaitingForInterpreter) {
    setInterpreterOwnership(jobWaitingForInterpreter, true, interrupt);
  }
}

// The callbacks service.call receives for one request: progress, commit, the
// external-wait bracket and the protected-frame entry.
function operationCallbacks(
  message: WorkerRequest,
  lane: PythonLane,
  control: Int32Array,
  interrupt: Uint8Array
) {
  // Returns false once cancelled, which tells Python to stop.
  const reportProgress = (done: number, total: number, phase: string): boolean => {
    if (cancellationRequested(control)) return false;
    postCountProgress(BigInt(message.jobId || 0), done, total < 0 ? null : total, phase);
    return true;
  };
  const beginRequestCommit = (): boolean => {
    return beginCommit(control, interrupt);
  };
  const beginExternalWait = (): void => {
    setExternalWait(control, true, false, interrupt);
  };
  // "Does this request own the interpreter" is lane-dependent: a view is the
  // owner exactly while it is the active view, a job exactly while no view
  // displaces it.
  // The job-only predicate would read inverted for a view.
  const ownsInterpreter = (): boolean =>
    lane === "VIEW" ? activeViewControl === control : activeViewControl == null;
  const endExternalWait = (): void => {
    setExternalWait(control, false, ownsInterpreter(), interrupt);
  };
  // Called by service.call from its first protected bytecodes.
  // It grants ownership (arming the interrupt if cancellation is already pending)
  // only once the target frame is provably the one executing.
  // See acquireInterpreter.
  const enterProtectedFrame = (): void => {
    setInterpreterOwnership(control, ownsInterpreter(), interrupt);
  };
  return {
    reportProgress,
    beginRequestCommit,
    beginExternalWait,
    endExternalWait,
    enterProtectedFrame,
  };
}

// The typed cancelled reply, produced worker-side for a request whose cancellation
// is decided before (or instead of) Python answering.
// That is a cancel that won while the request was queued, or a KeyboardInterrupt
// consumed outside service.call's protected frames.
function cancelledReply(): WorkerReplyPayload {
  const result = toBinary(
    BridgeResultSchema,
    create(BridgeResultSchema, {
      result: { case: "cancelled", value: create(CancelledSchema) },
    })
  );
  const resultBuffer = result.buffer as ArrayBuffer;
  return { result: resultBuffer, transfer: [resultBuffer] };
}

function textReply(value: string): WorkerReplyPayload {
  const result = toBinary(
    BridgeResultSchema,
    create(BridgeResultSchema, {
      result: { case: "text", value: create(StringValueSchema, { value }) },
    })
  );
  const resultBuffer = result.buffer as ArrayBuffer;
  return { result: resultBuffer, transfer: [resultBuffer] };
}

// True when an error thrown by service.call is this request's own cancellation
// surfacing outside the protected frames.
// Pyodide raises it as a PythonError whose `type` names the Python exception
// class.
// Gated on the control word, so an unrelated Python failure that merely mentions
// KeyboardInterrupt can never be misread as a cancellation.
function isEscapedCancellation(control: Int32Array, error: unknown): boolean {
  if (!cancellationRequested(control)) return false;
  const type = (error as { type?: unknown } | null)?.type;
  return type === "KeyboardInterrupt";
}

function destroyPyProxy(value) {
  if (value && typeof value.destroy === "function") value.destroy();
}

// Copy a Python list of strings into JS and release the proxy behind it.
function takePythonStringList(value): string[] {
  try {
    return value.toJs() as string[];
  } finally {
    destroyPyProxy(value);
  }
}

function copyBytes(value) {
  const source = value instanceof Uint8Array ? value : new Uint8Array(value.buffer || value);
  const copy = new Uint8Array(source.byteLength);
  copy.set(source);
  return copy;
}

// Copies bytes into JS-owned memory and destroys any Python proxy wrapper.
function takePythonBytes(value) {
  try {
    if (value instanceof Uint8Array) return copyBytes(value);
    if (value && typeof value.toJs === "function") return copyBytes(value.toJs());
    return copyBytes(value);
  } finally {
    destroyPyProxy(value);
  }
}

// Shape the {"result": BridgeResult bytes, "buffer"?: ndarray} dict that
// service.call returns into the worker's postMessage payload.
// The sample buffer (`ys`) is transferred only when present (getView).
function replyFromPython(pythonResult): WorkerReplyPayload {
  try {
    const result = takePythonBytes(pythonResult.get("result"));
    const resultBuffer = result.buffer as ArrayBuffer;
    const reply: WorkerReplyPayload = { result: resultBuffer, transfer: [resultBuffer] };
    if (pythonResult.has("buffer")) {
      const samplesProxy = pythonResult.get("buffer");
      let pythonBuffer: any = null;
      try {
        pythonBuffer = samplesProxy.getBuffer();
        const data = pythonBuffer.data;
        const samples = copyBytes(
          data instanceof Uint8Array
            ? data
            : new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
        );
        reply.ys = samples.buffer;
        reply.transfer.push(samples.buffer);
      } finally {
        if (pythonBuffer) pythonBuffer.release();
        destroyPyProxy(samplesProxy);
      }
    }
    return reply;
  } finally {
    destroyPyProxy(pythonResult);
  }
}

// Roughly how many modules importing the bridge pulls in, used only to shape the bar
// across the import phase.
// The count is measured, not derived, so it drifts as pyrocko/numpy change.
// progressFraction clamps at the end of the span, and the final report jumps the
// bar there regardless, so drift costs smoothness, never correctness.
const BOOT_MODULE_ESTIMATE = 700;

// Called from Python as modules come in (see bootstrap.import_bridge).
function bootModuleProgress(count: number) {
  reportBootProgress(
    progressFraction(BOOT_SPAN.importStart, BOOT_SPAN.importEnd, count, BOOT_MODULE_ESTIMATE),
    count >= BOOT_MODULE_ESTIMATE ? "Importing pyrocko…" : `Importing pyrocko… (${count} modules)`
  );
}

async function fetchPythonSources(
  files: Array<[string, string]>
): Promise<Array<[string, string]>> {
  return await Promise.all(
    files.map(
      async ([destination, path]): Promise<[string, string]> => [
        destination,
        await (await fetchOk(resolveUrl(path))).text(),
      ]
    )
  );
}

// Every module of the web/snuffler_bridge package, in dependency order.
// Spelled out because a static file server cannot list a directory.
// test_the_worker_stages_every_bridge_module (tests/parity) fails when this list
// and the package disagree.
const BRIDGE_PACKAGE_MODULES = [
  "__init__",
  "nslc",
  "operation",
  "marker_budget",
  "state",
  "fetch",
  "fdsn",
  "staging",
  "pyrocko_modules",
  "processing",
  "cutout",
  "sorting",
  "waterfall",
  "view",
  "snuffling_scope",
  "gf_stores",
  "events",
  "cake",
  "polarization",
  "figures",
  "seismosizer",
  "seisbench",
  "stalta",
  "rms",
  "snufflings",
  "waveforms",
  "stations",
  "bundles",
  "maps",
  "marker_files",
  "commands",
  "service",
];

// The Python sources a Shift+R reload re-fetches, as [Pyodide FS path, URL].
const RELOADABLE_PYTHON_SOURCES: Array<[string, string]> = [
  // Imported lazily by the bridge for SeisBench pre/post-processing.
  ["/seisbench_infer.py", "seisbench_infer.py"],
  ...BRIDGE_PACKAGE_MODULES.map((module): [string, string] => [
    `/snuffler_bridge/${module}.py`,
    `snuffler_bridge/${module}.py`,
  ]),
];

// The generated proto modules and the bridge package, staged into the Pyodide FS
// once Python is up.
// They are plain static files with no dependency on the runtime, so fetch them in
// parallel with it rather than one at a time after it.
async function fetchBridgeSources(): Promise<Array<[string, string]>> {
  const protoDir = "proto/snuffler_web_proto";
  return await fetchPythonSources([
    ...[
      "activity_pb2.py",
      "common_pb2.py",
      "scene_pb2.py",
      "view_pb2.py",
      "snuffling_pb2.py",
      "figure_pb2.py",
      "data_pb2.py",
      "bridge_pb2.py",
      // Generated typed servicer Protocol + dispatch (from BridgeService), which
      // the bridge imports, so it must be staged alongside the *_pb2 modules.
      "bridge_dispatch.py",
    ].map((file): [string, string] => [`/snuffler_web_proto/${file}`, `${protoDir}/${file}`]),
    // Imported before the bridge: it decides what pyrocko's own imports defer.
    ["/bootstrap.py", "bootstrap.py"],
    ...RELOADABLE_PYTHON_SOURCES,
  ]);
}

function makeDirectories(paths: string[]) {
  for (const path of paths) {
    try {
      pyodide.FS.mkdir(path);
    } catch (_e) {
      // Already exists.
    }
  }
}

// Runs the deployment's config.js, which sets `snufflerConfig` for the bridge to read
// (`web/config.js` documents it).
// The worker sits next to it, so it resolves against the worker's own URL.
// Without it the bridge runs with no proxy, as on a deployment that names none.
async function importDeploymentConfig(): Promise<void> {
  try {
    await import(new URL("config.js", self.location.href).href);
  } catch (err) {
    console.warn("[bridge-worker] config.js did not load; running without a proxy", err);
  }
}

// Boots Python once; every request awaits the same promise.
async function boot() {
  if (pyodideReadyPromise) return pyodideReadyPromise;
  pyodideReadyPromise = (async () => {
    try {
      const bootStartMs = performance.now();
      setBootPhase("Downloading Python runtime…");

      // Everything the boot downloads starts here, concurrently: the runtime, the
      // wheel closure, and our own Python sources.
      // Only the runtime is awaited before the interpreter starts; the rest lands
      // while it is coming up.
      const sources = fetchBridgeSources();
      void sources.catch(() => {});
      const deploymentConfig = importDeploymentConfig();
      const prefetch = await startPyodidePrefetch();
      await prefetch.priority;

      // Start the runtime.
      // Pyodide 314 rejects classic workers, so this is the ESM build, imported
      // from the only base there is: our locally served py-mt/.
      setBootPhase("Starting Python runtime…");
      const { loadPyodide } = await import(PYODIDE_BASE + "pyodide.mjs");
      pyodide = await loadPyodide({ indexURL: PYODIDE_BASE });
      const runtimeReadyMs = performance.now();

      // Install the packages.
      // Everything (incl. pyrocko, protobuf, requests, pyodide-http) is in
      // py-mt/pyodide-lock.json, built for this runtime, so load by name.
      // One at a time, so the bar advances per package (~60 ms slower than handing
      // loadPackage the whole list at once, measured).
      // The wheels are already in memory by now, so this is unpack + dlopen with no
      // network left to overlap.
      //
      // Progress is weighted by wheel size, not package count: unpacking costs
      // ~50 ms/MB, so scipy alone is over half this phase while 19 of the 23
      // packages together are under 5% of it.
      setBootPhase("Downloading Python packages…");
      await prefetch.all;
      setBootPhase("Installing Python packages…");
      const installed = prefetch.packages.map((pkg) => ({
        name: pkg.name,
        bytes: prefetchedByteLength(pkg.url),
      }));
      const installTotalBytes = installed.reduce((sum, pkg) => sum + pkg.bytes, 0);
      let installDoneBytes = 0;
      for (const pkg of installed) {
        await pyodide.loadPackage(pkg.name);
        installDoneBytes += pkg.bytes;
        reportBootProgress(
          progressFraction(
            BOOT_SPAN.installStart,
            BOOT_SPAN.importStart,
            installDoneBytes,
            installTotalBytes
          ),
          "Installing Python packages…"
        );
      }
      clearPrefetched();
      const packagesReadyMs = performance.now();

      // Stage our sources and import the bridge.
      // bootstrap.py defers the scipy imports pyrocko does at module scope but only
      // uses inside functions, then imports the bridge while counting modules, so
      // the longest phase of the boot has a moving bar instead of a frozen one.
      reportBootProgress(BOOT_SPAN.importStart, "Importing pyrocko…");
      await deploymentConfig;
      makeDirectories(["/snuffler_web_proto", "/snuffler_bridge"]);
      for (const [destination, source] of await sources) {
        pyodide.FS.writeFile(destination, source);
      }
      pyodide.runPython('import sys; sys.path.insert(0, "/"); import bootstrap');
      const deferred = takePythonStringList(pyodide.runPython("bootstrap.defer()"));
      const importBridge = pyodide.runPython("bootstrap.import_bridge");
      try {
        bridgeService = importBridge(bootModuleProgress);
      } finally {
        importBridge.destroy();
      }
      const importDoneMs = performance.now();

      // Boot-phase timings, logged once so regressions are visible in the console.
      // runtime = download + start the Pyodide runtime.
      // packages = loadPackage (numpy/scipy/matplotlib/pyrocko/…).
      // import = stage protos + import the bridge (which imports pyrocko).
      // The py-mt runtime also stands up libomp and its pthread workers, so these
      // spans include that spin-up.
      const spanMs = (fromMs: number, toMs: number) => Math.round(toMs - fromMs);
      console.log(
        `[bridge-worker] boot timing (ms): runtime=${spanMs(bootStartMs, runtimeReadyMs)} ` +
          `packages=${spanMs(runtimeReadyMs, packagesReadyMs)} ` +
          `import=${spanMs(packagesReadyMs, importDoneMs)} ` +
          `total=${spanMs(bootStartMs, importDoneMs)} deferred=${deferred.join(",") || "none"}`
      );

      // Report the booted runtime for the debug badge.
      try {
        postRuntimeInfo(pyodide);
      } catch (_e) {
        // Non-fatal: the badge just won't show.
      }
      try {
        postMlDevice(await mlDevice());
      } catch (_e) {
        postMlDevice(MlDevice.WASM);
      }

      reportBootProgress(BOOT_SPAN.almostReady, "Almost ready…");
      return pyodide;
    } catch (e) {
      console.error("[bridge-worker] boot failed:", e);
      throw e;
    }
  })();
  return pyodideReadyPromise;
}

// Re-fetch the Python sources and importlib.reload them, keeping the session
// (snuffler's Shift+R reloads code, not data).
// What survives is listed in `RELOAD_KEEP` in snuffler_bridge/state.py.
async function reloadSnufflings(): Promise<WorkerReplyPayload> {
  for (const [destination, source] of await fetchPythonSources(RELOADABLE_PYTHON_SOURCES)) {
    pyodide.FS.writeFile(destination, source);
  }
  // A module missing from sys.modules is new in this build, so it is imported
  // rather than reloaded.
  pyodide.runPython(`
import importlib, sys
from snuffler_bridge import state
if 'seisbench_infer' in sys.modules:
    importlib.reload(sys.modules['seisbench_infer'])
_saved = state.reload_snapshot()
importlib.invalidate_caches()
try:
    for _module in ${JSON.stringify(BRIDGE_PACKAGE_MODULES)}:
        _name = 'snuffler_bridge' if _module == '__init__' else 'snuffler_bridge.' + _module
        if _name in sys.modules:
            importlib.reload(sys.modules[_name])
        else:
            importlib.import_module(_name)
finally:
    # The snapshot detached the session state from the module dict, so restore
    # must run even when a reload raised.
    state.reload_restore(_saved)
`);
  return textReply("ok");
}

// Mounts the picked directory into the Pyodide FS and returns the mount point.
// Each store gets a fresh mount point, so remounting never collides.
async function mountGfStore(handle: FileSystemDirectoryHandle): Promise<string> {
  mountedGfStoreCount += 1;
  const mountPoint = "/gfstores/" + mountedGfStoreCount;
  makeDirectories(["/gfstores"]);
  pyodide.FS.mkdir(mountPoint);
  await pyodide.mountNativeFS(mountPoint, handle);
  return mountPoint;
}

// The worker is a relay: it forwards the generated method id, the request bytes
// and optional out-of-band data to the bridge's async dispatch, which runs the rpc
// and returns the BridgeResult envelope.
// MountGfStore's directory handle rides beside the protobuf request because
// browser handles cannot be serialized into it.
async function handleRequest(message: WorkerRequest): Promise<WorkerReplyPayload> {
  await boot();
  if (!pyodideInterrupt) {
    pyodideInterrupt = new Uint8Array(message.interrupt as SharedArrayBuffer);
    pyodide.setInterruptBuffer(pyodideInterrupt);
  }
  const interrupt = pyodideInterrupt;
  if (!interrupt) throw new Error("Pyodide interrupt buffer is unavailable");
  const control = executionControl(message.control as SharedArrayBuffer);
  const lane = acquireInterpreter(message, control, interrupt);
  const jobId = BigInt(message.jobId || 0);
  if (message.method === BridgeMethod.runSnuffling) setSeisbenchJob(jobId);
  postOperationStarted(jobId);
  const {
    reportProgress,
    beginRequestCommit,
    beginExternalWait,
    endExternalWait,
    enterProtectedFrame,
  } = operationCallbacks(message, lane, control, interrupt);

  // The single seam into the bridge for this request, shared by the GF-store mount
  // and the plain rpc so both pass the same callbacks and cancellation backstop.
  // `data` is the out-of-band argument: the GF store's mount point, or null.
  const callPython = async (data: string | null): Promise<WorkerReplyPayload> => {
    let pythonResult;
    try {
      pythonResult = await bridgeService.call(
        message.method,
        message.request,
        data,
        reportProgress,
        beginRequestCommit,
        beginExternalWait,
        endExternalWait,
        enterProtectedFrame
      );
    } catch (error) {
      if (isEscapedCancellation(control, error)) return cancelledReply();
      throw error;
    }
    return replyFromPython(pythonResult);
  };

  try {
    // A request cancelled while it was queued never reaches Python: dispatching
    // it would need SIGINT armed before its first protected frame, where the
    // interrupt could land in coroutine-scheduling plumbing instead.
    if (cancellationRequested(control)) return cancelledReply();
    if (message.method === WORKER_RELOAD_SNUFFLINGS) return await reloadSnufflings();
    if (message.method === BridgeMethod.mountGfStore) {
      if (!message.handle) {
        throw new Error("mounting a GF store requires a directory handle");
      }
      return await callPython(await mountGfStore(message.handle));
    }
    return await callPython(null);
  } finally {
    releaseInterpreter(lane, control, interrupt);
  }
}

// Booting Python is this worker's whole purpose and takes seconds, so start it on
// module load rather than waiting for the first rpc.
// The page creates the worker while the app wasm is still downloading (see
// index.html), and that head start is the entire Pyodide download.
// handleRequest awaits the same memoized promise, so a request that arrives
// mid-boot still queues behind it and surfaces its failure.
void boot().catch(postRuntimeFailure);

self.onmessage = (event: MessageEvent<WorkerRequest>) => {
  const message = event.data || {};
  if (message.epoch !== RUNTIME_EPOCH) {
    const response: WorkerFailureMessage = {
      epoch: RUNTIME_EPOCH,
      id: message.id,
      ok: false,
      error: `runtime epoch mismatch: worker=${RUNTIME_EPOCH} request=${String(message.epoch)}`,
    };
    self.postMessage(response);
    return;
  }
  const jobId = typeof message.jobId === "bigint" ? message.jobId : BigInt(message.jobId || 0);
  handleRequest(message)
    .then((reply) => {
      const { transfer, ...payload } = reply;
      const response: WorkerSuccessMessage = {
        epoch: RUNTIME_EPOCH,
        id: message.id,
        ok: true,
        ...payload,
      };
      self.postMessage(response, transfer);
    })
    .catch((e) => {
      const response: WorkerFailureMessage = {
        epoch: RUNTIME_EPOCH,
        id: message.id,
        ok: false,
        error: e && e.message ? e.message : String(e),
      };
      self.postMessage(response);
    })
    .finally(() => {
      clearSeisbenchJob(jobId);
    });
};
