// Downloads the Pyodide runtime and its wheels ahead of loadPyodide() and
// loadPackage(), as one concurrent, byte-metered batch.
// A `fetch` override then answers their own requests from that batch.
import { BOOT_SPAN, advanceBootFraction, progressFraction } from "./worker_events.js";

// Requests in flight while prefetching.
// Six matches what browsers allow per host over HTTP/1.1, and keeps a high-latency
// link busy instead of paying one round trip per wheel.
const PREFETCH_CONCURRENCY = 6;

// The packages the bridge imports; the lock file supplies their dependency closure.
// docker/pyodide-mt/check-lock.sh checks the lock against this list.
const PYODIDE_PACKAGES = [
  "numpy",
  "scipy",
  "matplotlib",
  "pyyaml",
  "micropip",
  "protobuf",
  "requests",
  "pyodide-http",
  "pyrocko",
];

const nativeFetch = self.fetch.bind(self) as typeof fetch;
const prefetchedResponses = new Map<string, Uint8Array>();

export function resolveUrl(path: string): string {
  return new URL(path, self.location.href).href;
}

export async function fetchOk(url: string): Promise<Response> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`fetch ${url}: ${response.status} ${response.statusText}`);
  return response;
}

// The size of a prefetched download, 0 once it has been served or cleared.
export function prefetchedByteLength(url: string): number {
  return prefetchedResponses.get(url)?.byteLength ?? 0;
}

// Drops whatever the boot downloaded but nobody asked for.
export function clearPrefetched() {
  prefetchedResponses.clear();
}

function normalizeFetchUrl(input: RequestInfo | URL): string {
  if (typeof input === "string") return new URL(input, self.location.href).href;
  if (input instanceof URL) return input.href;
  return new URL(input.url, self.location.href).href;
}

function responseHeadersFor(url: string, bytes: Uint8Array): Headers {
  const headers = new Headers();
  headers.set("content-length", String(bytes.byteLength));
  if (url.endsWith(".wasm")) headers.set("content-type", "application/wasm");
  else if (url.endsWith(".json")) headers.set("content-type", "application/json");
  else if (url.endsWith(".mjs") || url.endsWith(".js")) {
    headers.set("content-type", "text/javascript");
  } else if (url.endsWith(".zip")) headers.set("content-type", "application/zip");
  else headers.set("content-type", "application/octet-stream");
  return headers;
}

// Serve loadPyodide()/loadPackage()'s own fetches out of the batch downloaded ahead
// of them.
// Anything they ask for that was not prefetched falls through to the network.
// Each prefetched response is served once.
(self as any).fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = normalizeFetchUrl(input);
  const prefetched = prefetchedResponses.get(url);
  if (!prefetched) return await nativeFetch(input, init);
  prefetchedResponses.delete(url);
  return new Response(prefetched.slice(), {
    status: 200,
    headers: responseHeadersFor(url, prefetched),
  });
};

function responseContentLength(response: Response): number {
  const value = Number(response.headers.get("content-length") || 0);
  return Number.isFinite(value) && value > 0 ? value : 0;
}

// 0 when the size is unknown; the meter then simply undercounts the total.
async function fetchContentLength(url: string): Promise<number> {
  try {
    const response = await nativeFetch(url, { method: "HEAD" });
    return response.ok ? responseContentLength(response) : 0;
  } catch (_e) {
    return 0;
  }
}

// Downloads `url`, calling `onChunk` with each chunk's size as it arrives.
async function fetchBytes(url: string, onChunk: (bytes: number) => void): Promise<Uint8Array> {
  const response = await nativeFetch(url);
  if (!response.ok) throw new Error(`fetch ${url}: ${response.status} ${response.statusText}`);
  if (!response.body) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    onChunk(bytes.byteLength);
    return bytes;
  }

  const chunks: Uint8Array[] = [];
  let byteLength = 0;
  const reader = response.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (!value) continue;
    chunks.push(value);
    byteLength += value.byteLength;
    onChunk(value.byteLength);
  }

  const bytes = new Uint8Array(byteLength);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

// Download `urls` into `prefetchedResponses` with `PREFETCH_CONCURRENCY` requests
// in flight, metering all of them as one byte-accurate progress span.
//
// `priority` is a prefix of `urls` that boot() needs before the rest (the Pyodide
// runtime, so the interpreter can start while the wheels are still downloading).
// It is fetched first and awaited separately.
function startPrefetch(
  priority: string[],
  rest: string[],
  start: number,
  end: number
): { priority: Promise<void>; all: Promise<void> } {
  // A Set keeps first-insertion order, so the priority urls stay at the front of
  // the queue the downloaders below pull from.
  const urls = Array.from(new Set([...priority, ...rest]));
  const settled = new Map<string, { resolve: () => void; reject: (error: unknown) => void }>();
  const priorityReady = priority.map(
    (url) =>
      new Promise<void>((resolve, reject) => {
        settled.set(url, { resolve, reject });
      })
  );

  let loadedBytes = 0;
  let totalBytes = 0;
  const report = () => {
    if (totalBytes <= 0) return;
    advanceBootFraction(progressFraction(start, end, loadedBytes, totalBytes));
  };

  const all = (async () => {
    const lengths = await Promise.all(urls.map(fetchContentLength));
    totalBytes = lengths.reduce((sum, value) => sum + value, 0);

    // Each downloader takes the next url off the shared queue until it is empty.
    let nextIndex = 0;
    const downloader = async () => {
      for (;;) {
        const index = nextIndex++;
        if (index >= urls.length) return;
        const url = urls[index];
        try {
          const bytes = await fetchBytes(url, (chunkBytes) => {
            loadedBytes += chunkBytes;
            report();
          });
          prefetchedResponses.set(url, bytes);
          settled.get(url)?.resolve();
        } catch (e) {
          settled.get(url)?.reject(e);
          throw e;
        }
      }
    };
    const downloaderCount = Math.min(PREFETCH_CONCURRENCY, urls.length);
    await Promise.all(Array.from({ length: downloaderCount }, downloader));
    advanceBootFraction(end);
  })();

  // boot() awaits `priority` first and `all` later; without this sink a failure
  // that surfaces through `priority` would also land as an unhandled rejection.
  void all.catch(() => {});
  return { priority: Promise.all(priorityReady).then(() => undefined), all };
}

function pyodidePackageName(name: string): string {
  return name.toLowerCase().replace(/_/g, "-");
}

// PYODIDE_PACKAGES and everything they depend on, per the lock file, in
// depth-first order.
function pyodidePackageClosure(lock: any): string[] {
  const packages = lock.packages || {};
  const seen = new Set<string>();
  const closure: string[] = [];
  const visit = (name: string) => {
    const normalized = pyodidePackageName(name);
    if (seen.has(normalized)) return;
    const entry = packages[normalized];
    if (!entry) throw new Error(`pyodide package ${name} is missing from pyodide-lock.json`);
    seen.add(normalized);
    closure.push(normalized);
    for (const dependency of entry.depends || []) visit(String(dependency));
  };
  for (const name of PYODIDE_PACKAGES) visit(name);
  return closure;
}

// Start every download the boot needs, runtime first and then the wheel closure,
// as one metered, concurrent batch.
// The wheels are not needed until loadPackage(), so downloading them alongside the
// runtime start overlaps ~30 MB of network with the interpreter's own (CPU-bound)
// startup instead of queueing behind it.
export async function startPyodidePrefetch(): Promise<{
  priority: Promise<void>;
  all: Promise<void>;
  packages: Array<{ name: string; url: string }>;
}> {
  // loadPyodide() and loadPackage() both read the lock again; keep the bytes so
  // they are answered from the prefetch cache rather than a second round trip.
  const lockUrl = resolveUrl("py-mt/pyodide-lock.json");
  const lockBytes = new Uint8Array(await (await fetchOk(lockUrl)).arrayBuffer());
  prefetchedResponses.set(lockUrl, lockBytes);
  const lock = JSON.parse(new TextDecoder().decode(lockBytes));
  const entries = lock.packages || {};
  const packages = pyodidePackageClosure(lock).map((name) => ({
    name,
    url: resolveUrl(`py-mt/${entries[name].file_name}`),
  }));
  return {
    packages,
    ...startPrefetch(
      [
        resolveUrl("py-mt/pyodide.asm.mjs"),
        resolveUrl("py-mt/pyodide.asm.wasm"),
        resolveUrl("py-mt/python_stdlib.zip"),
      ],
      packages.map((pkg) => pkg.url),
      BOOT_SPAN.downloadStart,
      BOOT_SPAN.installStart
    ),
  };
}
