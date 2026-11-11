// Map tiles for the egui map view, fetched and decoded to RGBA on the main thread.

async function decodeTile(url: string): Promise<ImageData> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`tile ${url}: ${response.status}`);
  const bitmap = await createImageBitmap(await response.blob());
  const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
  const context = canvas.getContext("2d");
  if (!context) throw new Error("tile: 2d canvas context unavailable");
  context.drawImage(bitmap, 0, 0);
  const imageData = context.getImageData(0, 0, bitmap.width, bitmap.height);
  if (bitmap.close) bitmap.close();
  return imageData;
}

/**
 * Fetch and decode a whole batch of map tiles at once.
 *
 * One call crosses the wasm boundary and starts every fetch in the batch.
 * Each tile is handed back through `onTile` the moment it decodes, so a single
 * slow tile delays only itself and the map paints as its pieces land.
 *
 * A tile that fails (404 past a provider's max zoom, offline) is reported with
 * zero dimensions, so the caller can retire the slot instead of waiting on it
 * forever.
 * The promise settles once every tile is accounted for, which is the caller's
 * signal to start the next batch.
 */
export async function fetchTiles(
  urls: string[],
  onTile: (index: number, widthPx: number, heightPx: number, rgba: Uint8Array) => void
): Promise<void> {
  const empty = new Uint8Array(0);
  await Promise.all(
    urls.map(async (url, index) => {
      try {
        const imageData = await decodeTile(url);
        onTile(index, imageData.width, imageData.height, new Uint8Array(imageData.data.buffer));
      } catch {
        onTile(index, 0, 0, empty);
      }
    })
  );
}
