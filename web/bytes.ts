// A private copy of `value`, or null for no value.
// wasm-bindgen hands over views of the shared wasm heap, which `Blob` rejects and
// which must not be transferred, so every buffer leaving Rust is copied first.
export function copyBytes(
  value: Uint8Array | ArrayBufferLike | null | undefined
): Uint8Array<ArrayBuffer> | null {
  if (value == null) return null;
  const source = value instanceof Uint8Array ? value : new Uint8Array(value);
  const copy = new Uint8Array(source.byteLength);
  copy.set(source);
  return copy;
}
