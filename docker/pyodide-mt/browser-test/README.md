# In-browser validation of the threaded Pyodide 314 runtime

Proves the fork-free pthread+OpenMP+SIMD Pyodide 314 + full package set runs in a
real browser, cross-origin-isolated, with Pyodide in a Web Worker (the app's env).

Usage (after build.sh + packages.sh + build-pyrocko.sh have populated
`.work/pyodide/served-mt` = runtime + wheels + pyodide-lock.json):

    cp browser-test/{test.html,test-worker.mjs,server.py} .work/pyodide/served-mt/
    cd .work/pyodide/served-mt && python3 server.py 8488   # COOP/COEP headers
    # open http://127.0.0.1:8488/test.html in a SIMD+SAB browser (Chrome 91+/FF89+)

Verified result (Chrome, 48 cores):
    crossOriginIsolated=true
    pyodide 314.1.0.dev0 loaded, pthreads=True
    packages loaded: np2.4.3 sp1.17.0 mpl3.10.8 pyrocko2026.06.02
    parstack runs threaded, result bit-identical across thread counts.

Note: in-browser OpenMP speedup is workload-bound — each thread is a Web Worker
spawn (~ms), so light ops (tens of ms) see little gain while heavy stacking
scales (the extracted-kernel bench reaches 18.6x). hardwareConcurrency=48.
