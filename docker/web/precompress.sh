#!/bin/sh
# Precompress a static tree so nginx can answer from brotli_static/gzip_static:
# the client gets the smallest representation its Accept-Encoding allows, and the
# server spends no CPU per request compressing the same 12 MB of wasm again.
#
# brotli and gzip, and deliberately nothing else.
# zstd would be a third copy of every asset for no client benefit: for content
# compressed ONCE, offline, at max effort, `brotli -q 11` is the better ratio —
# zstd's advantage is compression speed, which is exactly the axis that stops
# mattering at build time — and every browser that negotiates zstd already
# negotiates br.
#
#   precompress.sh DIR
set -eu

# Recursive entry point: one file, so the sweep below can fan out across cores.
if [ "${1:-}" = "--one" ]; then
    src="$2"
    orig=$(wc -c <"$src")
    # A variant only earns its place if it saves something real.
    # Below this it costs image size and an extra stat per request to hand back
    # what the plain file would have.
    # It is also what keeps the ONNX weights honest: dense float32 tensors barely
    # compress, and this drops the copy when they don't.
    budget=$((orig - orig / 10))

    brotli -q 11 -f -k -o "$src.br" "$src"
    [ "$(wc -c <"$src.br")" -lt "$budget" ] || rm -f "$src.br"

    gzip -9 -k -f "$src"
    [ "$(wc -c <"$src.gz")" -lt "$budget" ] || rm -f "$src.gz"
    exit 0
fi

root="${1:?usage: precompress.sh DIR}"
self="$0"

# Compressing an already-compressed container is minutes of CPU spent looking for
# bytes that are not there: the wheels and python_stdlib.zip are deflate streams,
# the images and fonts carry their own codec.
# Skipping them by name matters most for the Pyodide runtime, which is ~50 MB of
# wheels.
# config.js is never compressed: a deployment mounts its own over the image's, and
# brotli_static and gzip_static would keep answering with the image's copy.
find "$root" -type f -size +512c ! -name config.js \
    ! -name '*.br' ! -name '*.gz' ! -name '*.zst' \
    ! -name '*.zip' ! -name '*.whl' ! -name '*.tar' \
    ! -name '*.png' ! -name '*.jpg' ! -name '*.jpeg' ! -name '*.webp' \
    ! -name '*.ico' ! -name '*.woff' ! -name '*.woff2' ! -name '*.mp4' \
    -print0 | xargs -0 -r -P "$(nproc)" -n 1 "$self" --one

before=$(du -sk "$root" | cut -f1)
kept=$(find "$root" -name '*.br' | wc -l)
echo "[precompress] $root: $kept brotli+gzip variants, ${before} KiB total"
