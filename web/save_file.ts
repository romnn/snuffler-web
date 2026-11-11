// Saving the app's exports (traces, markers, figures) to the user's disk.
import { copyBytes } from "./bytes.js";

// Every export goes through here: the web's stand-in for the save dialog Qt pops
// for each of its exports.
// showSaveFilePicker (Chromium, the same API family as the open pickers) lets the
// user name and place the file.
// On engines without it, or when the click's transient activation has expired, the
// download attribute still carries the suggested name.
async function saveBlob(blob: Blob, suggestedName: string) {
  const picker = (window as unknown as { showSaveFilePicker?: (o?: unknown) => Promise<any> })
    .showSaveFilePicker;
  if (typeof picker === "function") {
    try {
      const handle = await picker.call(window, { suggestedName });
      const writable = await handle.createWritable();
      await writable.write(blob);
      await writable.close();
      return;
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") return; // user cancelled
      // NotAllowedError (no user activation left) and the rest: fall through.
    }
  }

  // Download fallback
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = suggestedName;
  document.body.appendChild(link);
  link.click();
  setTimeout(() => {
    URL.revokeObjectURL(link.href);
    link.remove();
  }, 1000);
}

// Rust has already decoded the typed protobuf result before this DOM-only step.
export async function downloadFile(bytes, filename, mime) {
  const blob = new Blob([copyBytes(bytes) ?? new Uint8Array()], {
    type: mime || "application/octet-stream",
  });
  await saveBlob(blob, filename || "download.bin");
}

export async function savePng(bytes, filename) {
  const blob = new Blob([copyBytes(bytes) ?? new Uint8Array()], { type: "image/png" });
  await saveBlob(blob, filename || "snuffler.png");
}

export async function saveSvg(svg, filename) {
  const blob = new Blob([svg], { type: "image/svg+xml" });
  await saveBlob(blob, filename || "snuffler.svg");
}
