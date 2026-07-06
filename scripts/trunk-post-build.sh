#!/usr/bin/env bash
set -euo pipefail

source_dir="${TRUNK_SOURCE_DIR:?}"
staging_dir="${TRUNK_STAGING_DIR:?}"
dist_dir="${TRUNK_DIST_DIR:?}"

build_dirs=()
for dir in "$staging_dir" "$dist_dir"; do
    if [ -d "$dir" ]; then
        build_dirs+=("$dir")
    fi
done

stage_ort_runtime() {
    local ort_dir="$source_dir/node_modules/onnxruntime-web/dist"
    local manifest="$source_dir/web/assets/seisbench_models.json"

    for dir in "${build_dirs[@]}"; do
        mkdir -p "$dir/ort" "$dir/assets"
        cp -u \
            "$ort_dir/ort-wasm-simd-threaded.asyncify.wasm" \
            "$ort_dir/ort-wasm-simd-threaded.jsep.wasm" \
            "$ort_dir/ort-wasm-simd-threaded.wasm" \
            "$dir/ort/"
        cp -u "$manifest" "$dir/assets/seisbench_models.json"
    done

    echo "[ort] runtime and SeisBench manifest staged"
}

stage_seisbench_models() {
    if [ -z "${SNUFFLER_EMBED_DATA:-}" ]; then
        echo "[seisbench] model weights not embedded"
        return
    fi

    local src="$source_dir/web/assets/models"
    if [ ! -d "$src" ]; then
        echo "[seisbench] no exported model weights found"
        return
    fi

    for dir in "${build_dirs[@]}"; do
        mkdir -p "$dir/assets/models"
        cp -u "$src"/*.onnx "$dir/assets/models/"
    done

    echo "[seisbench] model weights staged"
}

optimize_app_wasm() {
    local optimized=()

    for wasm in "$staging_dir"/*_bg.wasm; do
        [ -f "$wasm" ] || continue

        local tmp="${wasm}.opt"
        mise exec 'github:WebAssembly/binaryen' -- wasm-opt -Oz -all -o "$tmp" "$wasm"
        mv "$tmp" "$wasm"
        optimized+=("$(basename "$wasm")")
    done

    if [ -d "$dist_dir" ]; then
        for name in "${optimized[@]}"; do
            cp "$staging_dir/$name" "$dist_dir/$name"
        done
    fi
}

patch_rayon_worker_import() {
    for dir in "${build_dirs[@]}"; do
        local main
        main=$(basename "$dir"/snuffler-web-*.js 2>/dev/null) || continue

        for worker_helpers in "$dir"/snippets/wasm-bindgen-rayon-*/src/workerHelpers.js; do
            [ -f "$worker_helpers" ] || continue
            sed -i "s#import('../../..')#import('../../../$main')#" "$worker_helpers"
            echo "[rayon] patched worker module import -> $main"
        done
    done
}

patch_trunk_loader_progress() {
    local indexes=()

    for dir in "${build_dirs[@]}"; do
        if [ -f "$dir/index.html" ]; then
            indexes+=("$dir/index.html")
        fi
    done

    if [ "${#indexes[@]}" -gt 0 ]; then
        bun "$source_dir/scripts/patch-trunk-loader.mjs" "${indexes[@]}"
    fi
}

stage_threaded_pyodide() {
    local src="$source_dir/docker/pyodide-mt/served"

    if [ ! -d "$src" ]; then
        echo "[py-mt] not staged (run docker/pyodide-mt/stage-web.sh) - single-threaded CDN only"
        return
    fi

    for dir in "${build_dirs[@]}"; do
        mkdir -p "$dir/py-mt"
        cp -ru "$src/." "$dir/py-mt/"
    done

    echo "[py-mt] threaded runtime staged"
}

stage_ort_runtime
stage_seisbench_models
optimize_app_wasm
patch_rayon_worker_import
patch_trunk_loader_progress
stage_threaded_pyodide
