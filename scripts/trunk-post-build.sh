#!/usr/bin/env bash
set -euo pipefail

source_dir="${TRUNK_SOURCE_DIR:?}"
repo_dir="$source_dir/../.."
staging_dir="${TRUNK_STAGING_DIR:?}"
dist_dir="${TRUNK_DIST_DIR:?}"

build_dirs=()
for dir in "$staging_dir" "$dist_dir"; do
    if [ -d "$dir" ]; then
        build_dirs+=("$dir")
    fi
done

# Copy only what changed, so a rebuild does not re-copy the big staged payloads
# (the ONNX weights, the ~54 MB Pyodide runtime). This is `cp -u` by hand because
# that flag is GNU-only: the BSD cp on a macOS dev box rejects it outright.
# Size is checked as well as mtime: `-nt` alone trusts the clock, and a source
# that changed WITHOUT becoming newer — restored from a backup, checked out at an
# older revision, or rewritten by a tool that preserves mtimes — would otherwise
# leave the previous file in dist/. A stale py-mt wheel there does not fail loudly;
# it hangs the app at boot, so the extra stat is worth it.
copy_if_newer() {
    local src="$1" dst="$2"

    if [ ! -e "$dst" ] || [ "$src" -nt "$dst" ] ||
        [ "$(stat -f%z "$src" 2>/dev/null || stat -c%s "$src")" \
            != "$(stat -f%z "$dst" 2>/dev/null || stat -c%s "$dst")" ]; then
        cp "$src" "$dst"
    fi
}

# `cp -ru src/. dst/` by hand, for the same reason.
copy_tree_if_newer() {
    local src="$1" dst="$2" rel

    while IFS= read -r -d '' rel; do
        mkdir -p "$dst/$(dirname "$rel")"
        copy_if_newer "$src/$rel" "$dst/$rel"
    done < <(cd "$src" && find . -type f -print0)
}

# The core crate's wasm externs import bridge.js through a crate-escaping module
# path (#[wasm_bindgen(module = "/../../web/gen/bridge.js")] in
# crates/snuffler-web-core/src/bridge.rs). wasm-bindgen cannot embed a file from
# outside the crate as a snippet, so the emitted import resolves to
# web/gen/bridge.js under the site root — stage the bundle there.
stage_bridge_module() {
    local bridge="$repo_dir/web/gen/bridge.js"

    for dir in "${build_dirs[@]}"; do
        mkdir -p "$dir/web/gen"
        copy_if_newer "$bridge" "$dir/web/gen/bridge.js"
    done

    echo "[bridge] wasm-bindgen module staged"
}

stage_ort_runtime() {
    local ort_dir="$repo_dir/node_modules/onnxruntime-web/dist"
    local manifest="$repo_dir/web/assets/seisbench_models.json"

    for dir in "${build_dirs[@]}"; do
        mkdir -p "$dir/ort" "$dir/assets"
        for runtime in \
            ort-wasm-simd-threaded.asyncify.wasm \
            ort-wasm-simd-threaded.jsep.wasm \
            ort-wasm-simd-threaded.wasm; do
            copy_if_newer "$ort_dir/$runtime" "$dir/ort/$runtime"
        done
        copy_if_newer "$manifest" "$dir/assets/seisbench_models.json"
    done

    echo "[ort] runtime and SeisBench manifest staged"
}

# The ONNX weights named by web/assets/seisbench_models.json, in manifest order.
manifest_model_files() {
    SNUFFLER_SEISBENCH_MANIFEST="$1" bun -e '
        const manifest = JSON.parse(
            require("fs").readFileSync(process.env.SNUFFLER_SEISBENCH_MANIFEST, "utf8"),
        );
        for (const model of manifest.models ?? []) {
            if (model.file) console.log(model.file);
        }
    '
}

# Staging the weights is OPT-OUT, because the cargo feature that makes the app ask
# for them — `embed-data` — is on by DEFAULT. Only the lean build turns that
# feature off (and points the app at a remote SNUFFLER_ONNX_MODEL_BASE_URL
# instead), so only the lean build sets SNUFFLER_EMBED_DATA=0.
#
# The polarity is the whole point: as opt-in, the two switches disagreed silently
# in exactly one direction — default features (app requests assets/models/) with
# the env unset (nothing copied) — which is what the GitHub Pages workflow did,
# since it drives trunk directly and never saw the mise task's env. The deploy
# succeeded and every pick run 404'd. Getting it wrong the other way round only
# costs ~14 MB of unused files in dist.
stage_seisbench_models() {
    local manifest="$repo_dir/web/assets/seisbench_models.json"
    local src="$repo_dir/web/assets/models"

    if [ "${SNUFFLER_EMBED_DATA:-1}" = "0" ]; then
        echo "[seisbench] model weights not embedded (SNUFFLER_EMBED_DATA=0)"
        return
    fi

    # Read the list into a variable rather than piping the loop: a manifest that
    # cannot be read must abort (`set -e` on this assignment), not feed an empty
    # loop that then reports success with nothing copied.
    local files
    files=$(manifest_model_files "$manifest")
    if [ -z "$files" ]; then
        echo "[seisbench] ERROR: $manifest names no model files" >&2
        exit 1
    fi

    # Every file the manifest names must ship: the app resolves the model URL from
    # the manifest, so a missing or half-exported web/assets/models is a 404 at
    # pick time — a runtime failure for users, from a build that looked green.
    local missing=()
    while IFS= read -r model; do
        [ -n "$model" ] || continue
        if [ -f "$src/$model" ]; then
            for dir in "${build_dirs[@]}"; do
                mkdir -p "$dir/assets/models"
                copy_if_newer "$src/$model" "$dir/assets/models/$model"
            done
        else
            missing+=("$model")
        fi
    done <<<"$files"

    if [ "${#missing[@]}" -gt 0 ]; then
        echo "[seisbench] ERROR: seisbench_models.json names weights that are not in" >&2
        echo "  $src:" >&2
        for model in "${missing[@]}"; do
            echo "    $model" >&2
        done
        echo "  The default build has the \`embed-data\` feature on, so the app will" >&2
        echo "  request assets/models/<file> and 404. Export the weights, or build" >&2
        echo "  lean (mise run build-lean, --no-default-features + a remote" >&2
        echo "  SNUFFLER_ONNX_MODEL_BASE_URL)." >&2
        exit 1
    fi

    echo "[seisbench] model weights staged"
}

# `-Oz` only buys download size, which no one is paying for over localhost, and it
# is the single most expensive step of a rebuild (~60s on the app wasm) — it runs
# on every save in trunk's watch loop, where it is pure latency. So it is skipped
# for anything but a release build; `mise run serve` builds with the `serve` cargo
# profile and TRUNK_PROFILE is `debug` there, while `mise run build` (and CI/Pages)
# passes --release and still gets the fully optimized wasm.
optimize_app_wasm() {
    local optimized=()

    if [ "${TRUNK_PROFILE:-debug}" != "release" ]; then
        echo "[wasm-opt] skipped (TRUNK_PROFILE=${TRUNK_PROFILE:-unset}; release builds only)"
        return
    fi

    for wasm in "$staging_dir"/*_bg.wasm; do
        [ -f "$wasm" ] || continue

        local tmp="${wasm}.opt"
        mise exec 'aqua:WebAssembly/binaryen' -- wasm-opt -Oz -all -o "$tmp" "$wasm"
        mv "$tmp" "$wasm"
        optimized+=("$(basename "$wasm")")
    done

    # The array guard is for bash 3.2 (what macOS ships), where expanding an
    # empty array under `set -u` is an error rather than nothing.
    if [ -d "$dist_dir" ] && [ "${#optimized[@]}" -gt 0 ]; then
        for name in "${optimized[@]}"; do
            cp "$staging_dir/$name" "$dist_dir/$name"
        done
    fi
}

# Point wasm-bindgen-rayon's worker helper at the real (hashed) main JS module,
# and give the helper itself a per-build name.
#
# wasm-bindgen emits the helper at a path that is STABLE across builds, while the
# rewrite below makes its contents build-SPECIFIC — a stable url serving changing
# bytes, which is exactly what an HTTP cache is entitled to get wrong. GitHub Pages
# serves everything with its own max-age and gives no way to override the headers,
# so for a while after each deploy a returning browser can pair a cached helper
# with a freshly hashed module. The result is worse than stale: wasm-bindgen-rayon
# hands each worker the `WebAssembly.Module` already compiled on the main thread,
# so the worker links current wasm against a previous build's JS shim and the whole
# pool dies with a LinkError naming whichever import was added most recently.
# Renaming the helper per build keeps its url changing in step with its contents,
# which is the only thing a cache can be expected to honour.
#
# Rewriting the already-hashed main module means its name no longer matches its
# bytes, so this requires the --no-sri builds (see mise.toml).
patch_rayon_worker_import() {
    for dir in "${build_dirs[@]}"; do
        local main hash
        main=$(basename "$dir"/snuffler-web-*.js 2>/dev/null) || continue
        # snuffler-web-egui-<hash>.js -> <hash>
        hash=${main##*-}
        hash=${hash%.js}

        for worker_helpers in "$dir"/snippets/wasm-bindgen-rayon-*/src/workerHelpers.js; do
            [ -f "$worker_helpers" ] || continue
            local snippet_dir helper
            snippet_dir=$(dirname "$worker_helpers")
            helper="workerHelpers-$hash.js"

            # Drop earlier builds' helpers so dist does not accumulate them.
            rm -f "$snippet_dir"/workerHelpers-*.js

            # Edit through a temp file: `sed -i` takes no argument on GNU sed but
            # requires a (possibly empty) backup suffix on the BSD sed shipped
            # with macOS, so no single spelling works on both. The helper spawns
            # itself by name, so that reference has to move with the file.
            sed -e "s#import('../../..')#import('../../../$main')#" \
                -e "s#new URL('./workerHelpers.js'#new URL('./$helper'#" \
                "$worker_helpers" > "$snippet_dir/$helper"
            rm -f "$worker_helpers"

            # Both the main module (which imports the snippet) and index.html (which
            # preloads it) name the helper, and a missed one is not harmless: trunk
            # serve answers unknown paths with index.html, so the stale name resolves
            # to an HTML document that the browser then refuses as a module script.
            for referrer in "$dir/$main" "$dir/index.html"; do
                [ -f "$referrer" ] || continue
                sed "s#src/workerHelpers.js#src/$helper#g" \
                    "$referrer" > "$referrer.patched"
                mv "$referrer.patched" "$referrer"
            done

            echo "[rayon] worker helper -> $helper (imports $main)"
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
        bun "$repo_dir/scripts/patch-trunk-loader.mjs" "${indexes[@]}"
    fi
}

# The example datasets behind `File ▸ Examples`, packed from examples/ into one
# archive each. Staged like the SeisBench weights and for the same reason: the app
# fetches them only when someone picks one, so they cost a page load nothing and
# never enter the wasm bundle. The builder is deterministic and rewrites a bundle
# only when its contents change, so copy_if_newer stays quiet across rebuilds.
stage_examples() {
    local built="$repo_dir/target/examples"

    if ! python3 "$repo_dir/scripts/build-examples.py" --out "$built"; then
        echo "[examples] ERROR: could not pack examples/ — the Examples menu would 404" >&2
        exit 1
    fi

    for dir in "${build_dirs[@]}"; do
        mkdir -p "$dir/assets/examples"
        copy_tree_if_newer "$built" "$dir/assets/examples"
    done

    echo "[examples] dataset bundles staged"
}

stage_threaded_pyodide() {
    local src="$repo_dir/docker/pyodide-mt/served"

    if [ ! -d "$src" ]; then
        echo "[py-mt] MISSING - the app will hang at boot; build it with docker/pyodide-mt/build-all.sh"
        return
    fi

    for dir in "${build_dirs[@]}"; do
        mkdir -p "$dir/py-mt"
        copy_tree_if_newer "$src" "$dir/py-mt"
    done

    echo "[py-mt] threaded runtime staged"
}

stage_bridge_module
stage_ort_runtime
stage_seisbench_models
stage_examples
optimize_app_wasm
patch_rayon_worker_import
patch_trunk_loader_progress
stage_threaded_pyodide
