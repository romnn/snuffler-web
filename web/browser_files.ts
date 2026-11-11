// Reading the user's files into the app: the open pickers, paged waveform loads,
// "reload modified files", and the single-shot auxiliary imports (markers,
// events, stations, StationXML, cake models).
// Rust drives every load; this module only touches the DOM and hands back
// BrowserFileLoadPage protobufs.
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  BrowserFileLoadPageRequestSchema,
  BrowserFileLoadPageSchema,
  BrowserFileSchema,
  BrowserReadFailureSchema,
  BrowserSourceAckListSchema,
  type BrowserFile,
  type BrowserReadFailure,
} from "./proto/snuffler_web_proto/data_pb.js";

// The File System Access API, which only Chromium ships.
declare global {
  interface Window {
    showDirectoryPicker?: (opts?: { mode?: string }) => Promise<unknown>;
    showOpenFilePicker?: (opts?: { multiple?: boolean }) => Promise<FileSystemFileHandle[]>;
  }
}

// One page of a load holds at most this many bytes and files, and no single file
// may exceed the byte bound.
const FILE_PAGE_MAX_BYTES = 32 * 1024 * 1024;
const FILE_PAGE_MAX_FILES = 64;

// Re-readable handles for files opened via the File System Access API, keyed by a
// stable source id, so "reload modified files" can re-read them from disk.
// `mtime` is the last-modified time Rust acknowledged loading; 0 until then.
type SourceKind = "waveform" | "bundle";
type OpenSource = { handle: FileSystemFileHandle; mtime: number; kind: SourceKind };
const openSources = new Map<string, OpenSource>();
let nextSourceNumber = 0;

// One file to read: a File snapshot from <input> (never reloadable), or a handle.
// A reload sets `previousMtime`, and an unchanged file is then skipped.
type BrowserFileSource =
  | { case: "snapshot"; sourceId: ""; file: File }
  | {
      case: "handle";
      sourceId: string;
      handle: FileSystemFileHandle;
      file?: File;
      kind?: SourceKind;
      previousMtime?: number;
    };

// A load in progress, read one page per call.
// `pending` is the source that did not fit on the previous page.
type BrowserFileLoad = {
  iterator: AsyncIterator<BrowserFileSource>;
  pending?: BrowserFileSource;
  isDone: boolean;
};
const fileLoads = new Map<string, BrowserFileLoad>();
let nextFileLoadNumber = 0;

function newSourceId(): string {
  return `src${nextSourceNumber++}`;
}

function cancelledFileLoadPage(): Uint8Array {
  return toBinary(
    BrowserFileLoadPageSchema,
    create(BrowserFileLoadPageSchema, { cancelled: true })
  );
}

function readFailure(sourceId: string, name: string, detail: string): BrowserReadFailure {
  return create(BrowserReadFailureSchema, { sourceId, name, detail });
}

function oversizedFileDetail(file: File): string {
  return `file is ${file.size} bytes; the per-file limit is ${FILE_PAGE_MAX_BYTES}`;
}

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function clearOpenSources() {
  openSources.clear();
  fileLoads.clear();
}

// The <input type=file> picker, for browsers without showOpenFilePicker.
// It resolves with no files when the user dismisses it: a cancelled dialog fires
// no event, so the window regaining focus with nothing chosen counts as a cancel.
function pickFiles(multiple): Promise<File[]> {
  return new Promise<File[]>((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = !!multiple;
    input.style.display = "none";
    let isDone = false;
    const finish = (files: File[]) => {
      if (isDone) return;
      isDone = true;
      window.removeEventListener("focus", onFocus);
      input.remove();
      resolve(files);
    };
    input.addEventListener("change", () => finish(input.files ? Array.from(input.files) : []), {
      once: true,
    });
    const onFocus = () =>
      setTimeout(() => {
        if (!isDone && (!input.files || input.files.length === 0)) finish([]);
      }, 500);
    window.addEventListener("focus", onFocus);
    document.body.appendChild(input);
    input.click();
  });
}

// A picked file may be a whole dataset rather than one waveform file.
// Zips go to LoadBundle, which loads every member and returns the station/event
// results alongside the scene.
// Everything else keeps the plain per-file path.
const ZIP_MAGIC = [0x50, 0x4b, 0x03, 0x04];

function isZip(data: Uint8Array) {
  return data.length >= 4 && ZIP_MAGIC.every((byte, i) => data[i] === byte);
}

async function* fileSources(files: File[]): AsyncGenerator<BrowserFileSource> {
  for (const file of files) yield { case: "snapshot", sourceId: "", file };
}

async function* handleSources(handles: FileSystemFileHandle[]): AsyncGenerator<BrowserFileSource> {
  for (const handle of handles) {
    yield { case: "handle", sourceId: newSourceId(), handle };
  }
}

// Reads sources until the page is full or the load runs out.
// A failure to read one file becomes a typed failure on the page, not an error.
async function readNextFileLoadPage(loadId: string): Promise<Uint8Array> {
  const load = fileLoads.get(loadId);
  if (!load) throw new Error(`unknown browser file load ${loadId}`);
  const files: BrowserFile[] = [];
  const failures: BrowserReadFailure[] = [];
  let pageBytes = 0;

  while (files.length + failures.length < FILE_PAGE_MAX_FILES) {
    // Next source: the one held back from the previous page first.
    let source = load.pending;
    load.pending = undefined;
    if (!source) {
      const next = await load.iterator.next();
      if (next.done) {
        load.isDone = true;
        break;
      }
      source = next.value;
    }

    try {
      const file =
        source.case === "snapshot" ? source.file : (source.file ?? (await source.handle.getFile()));

      // Reload of an unchanged file
      if (
        source.case === "handle" &&
        source.previousMtime != null &&
        file.lastModified === source.previousMtime
      ) {
        continue;
      }

      // Too big for any page
      if (file.size > FILE_PAGE_MAX_BYTES) {
        failures.push(readFailure(source.sourceId, file.name, oversizedFileDetail(file)));
        continue;
      }

      // Too big for this page: hold it for the next one.
      if (pageBytes > 0 && pageBytes + file.size > FILE_PAGE_MAX_BYTES) {
        load.pending = { ...source, file };
        break;
      }

      const data = new Uint8Array(await file.arrayBuffer());
      const isBundle = (source.case === "handle" && source.kind === "bundle") || isZip(data);
      // A newly opened handle becomes reloadable; Rust acknowledges its mtime once
      // it has loaded (acknowledgeSources).
      if (source.case === "handle" && source.previousMtime == null) {
        openSources.set(source.sourceId, {
          handle: source.handle,
          mtime: 0,
          kind: isBundle ? "bundle" : "waveform",
        });
      }
      pageBytes += data.byteLength;
      files.push(
        create(BrowserFileSchema, {
          sourceId: source.sourceId,
          name: file.name,
          content: data,
          lastModified: BigInt(file.lastModified),
          bundle: isBundle,
        })
      );
    } catch (error) {
      if (source.case === "handle" && source.previousMtime == null) {
        openSources.delete(source.sourceId);
      }
      const name = source.case === "handle" ? source.handle.name : source.file.name;
      failures.push(readFailure(source.sourceId, name, errorDetail(error)));
    }
  }

  const hasMore = !load.isDone || load.pending != null;
  if (!hasMore) fileLoads.delete(loadId);
  return toBinary(
    BrowserFileLoadPageSchema,
    create(BrowserFileLoadPageSchema, { files, failures, loadId, hasMore })
  );
}

async function startFileLoad(iterator: AsyncIterator<BrowserFileSource>): Promise<Uint8Array> {
  const loadId = `load${nextFileLoadNumber++}`;
  fileLoads.set(loadId, { iterator, isDone: false });
  return await readNextFileLoadPage(loadId);
}

export async function readFileLoadPage(requestBytes: Uint8Array): Promise<Uint8Array> {
  const request = fromBinary(BrowserFileLoadPageRequestSchema, requestBytes);
  return await readNextFileLoadPage(request.loadId);
}

export function cancelFileLoad(requestBytes: Uint8Array): void {
  const request = fromBinary(BrowserFileLoadPageRequestSchema, requestBytes);
  fileLoads.delete(request.loadId);
}

export async function openFiles() {
  // Prefer showOpenFilePicker (Chromium): it yields re-readable handles so the
  // files can later be reloaded from disk.
  // Fall back to a hidden <input> (File snapshots, no reload) on browsers
  // without the File System Access API.
  if (typeof window.showOpenFilePicker === "function") {
    let handles: FileSystemFileHandle[];
    try {
      handles = await window.showOpenFilePicker({ multiple: true });
    } catch {
      return cancelledFileLoadPage();
    }
    return await startFileLoad(handleSources(handles));
  }
  const files = await pickFiles(true);
  if (files.length === 0) {
    return cancelledFileLoadPage();
  }
  return await startFileLoad(fileSources(files));
}

// Recursively load every file under a picked directory (snuffler's "Open waveform
// directory").
// No extension filter: snuffler hands every file to format detection, which is
// what lets the extensionless SDS and day-file layouts (GE.WLF..BHZ.D.2023.037,
// 2009.123) load.
// Whatever detection rejects is skipped per file.
// Chromium-only (showDirectoryPicker); handles are retained so the loaded files
// also participate in reload.
export async function openDirectory() {
  const pick = (window as unknown as { showDirectoryPicker?: (o?: unknown) => Promise<any> })
    .showDirectoryPicker;
  if (typeof pick !== "function") {
    throw new Error(
      "This browser has no File System Access API (showDirectoryPicker); " +
        "opening a waveform directory needs a Chromium-based browser."
    );
  }
  let root: any;
  try {
    root = await pick({ mode: "read" });
  } catch {
    return cancelledFileLoadPage();
  }
  async function* walk(directory: any): AsyncGenerator<any> {
    for await (const [, entry] of directory.entries()) {
      if (entry.kind === "file") yield entry;
      else if (entry.kind === "directory") yield* walk(entry);
    }
  }
  async function* directorySources(): AsyncGenerator<BrowserFileSource> {
    for await (const handle of walk(root)) {
      yield { case: "handle", sourceId: newSourceId(), handle };
    }
  }
  return await startFileLoad(directorySources());
}

// Re-read any opened files whose on-disk mtime changed and replace their traces
// (snuffler's "r" reload).
// Each changed source commits independently, so a later failure cannot hide an
// earlier successful scene or make that success impossible to observe.
export async function reloadFiles() {
  if (typeof window.showOpenFilePicker !== "function") {
    throw new Error(
      "This browser has no File System Access API (showOpenFilePicker); " +
        "reloading files from disk needs a Chromium-based browser."
    );
  }
  async function* reloadSources(): AsyncGenerator<BrowserFileSource> {
    for (const [sourceId, source] of Array.from(openSources)) {
      yield {
        case: "handle",
        sourceId,
        handle: source.handle,
        kind: source.kind,
        previousMtime: source.mtime,
      };
    }
  }
  return await startFileLoad(reloadSources());
}

// Rust reports which sources it loaded, with the mtime it saw.
// A bundle replaces the whole session, so it also forgets every other source.
export function acknowledgeSources(bytes: Uint8Array) {
  const acknowledgements = fromBinary(BrowserSourceAckListSchema, bytes);
  for (const source of acknowledgements.sources) {
    const current = openSources.get(source.sourceId);
    if (!current) continue;
    if (source.bundle) openSources.clear();
    openSources.set(source.sourceId, {
      handle: current.handle,
      mtime: Number(source.lastModified),
      kind: source.bundle ? "bundle" : "waveform",
    });
  }
}

// The auxiliary imports are pickers only: Rust issues the rpc itself, and
// re-checks its ledger identity between this unbounded dialog wait and the call.

export async function pickCakeModel() {
  const files = await pickFiles(false);
  return pickedFilePage(files);
}

export async function loadMarkers() {
  const files = await pickFiles(false);
  return pickedFilePage(files);
}

export async function loadStationXml() {
  const files = await pickFiles(true);
  return pickedFilePage(files);
}

export async function loadStations() {
  // Any number of station files in one gesture, as Qt's picker takes them; the
  // format is line-based, so the documents concatenate into one table.
  const files = await pickFiles(true);
  return pickedFilePage(files);
}

export async function loadEvents() {
  const files = await pickFiles(false);
  return pickedFilePage(files);
}

// The auxiliary imports arrive as one aggregate rpc, so they carry the paged
// loader's byte bounds, per file and in total.
// Otherwise an oversized selection would still aggregate unbounded buffers on the
// main thread.
// Over-budget files become typed failures, and the Rust consumer is
// all-or-error, so one rejection fails the whole import with the reason spelled
// out.
async function pickedFilePage(files: File[]) {
  if (files.length === 0) {
    return cancelledFileLoadPage();
  }
  const loaded: BrowserFile[] = [];
  const failures: BrowserReadFailure[] = [];
  let totalBytes = 0;
  for (const file of files) {
    if (file.size > FILE_PAGE_MAX_BYTES) {
      failures.push(readFailure("", file.name, oversizedFileDetail(file)));
      continue;
    }
    if (totalBytes + file.size > FILE_PAGE_MAX_BYTES) {
      failures.push(
        readFailure(
          "",
          file.name,
          `the selection exceeds the ${FILE_PAGE_MAX_BYTES}-byte total limit for one import`
        )
      );
      continue;
    }
    totalBytes += file.size;
    try {
      loaded.push(
        create(BrowserFileSchema, {
          name: file.name,
          content: new Uint8Array(await file.arrayBuffer()),
          lastModified: BigInt(file.lastModified),
        })
      );
    } catch (error) {
      failures.push(readFailure("", file.name, errorDetail(error)));
    }
  }
  return toBinary(
    BrowserFileLoadPageSchema,
    create(BrowserFileLoadPageSchema, { files: loaded, failures })
  );
}
