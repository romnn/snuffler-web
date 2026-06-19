# Plan (LATER): cut first-load size by dropping matplotlib + scipy

> Status: **future work — do not implement now.** This documents the current
> download footprint and two concrete ways to shrink it. Captured 2026-06-19.

## Why

The app's first visit downloads ~**100 MB** from the Pyodide CDN. That's the
CPython runtime + the scientific stack, *not* our code. After the first load the
browser caches it, but first-load latency is dominated by two packages we barely
use: **scipy** (filtering only) and **matplotlib** (one stray import).

## Current footprint (measured)

What *we* build and host (~5 MB):

| Piece | Size |
|---|---|
| egui frontend wasm (the UI, release + wasm-opt) | 3.7 MB |
| pyrocko wheel (our cross-compile) | 1.2 MB |
| bridge.py + bundled test.mseed | ~0.6 MB |

The Python stack from the Pyodide CDN, downloaded once then browser-cached:

| Piece | Size | Needed for |
|---|---|---|
| `pyodide.asm.wasm` (CPython interpreter) | 9.6 MB | everything |
| `python_stdlib.zip` | 2.2 MB | everything |
| numpy | 11.9 MB | everything (arrays) |
| openblas (BLAS/LAPACK) | 5.8 MB | numpy dependency |
| **scipy** | **45.8 MB** | **only `Trace` Butterworth filtering** |
| **matplotlib** | **15.9 MB** | **only one import in `orthodrome`** |
| matplotlib deps (pillow 2.9, fonttools 4.1, contourpy 0.4, kiwisolver, cycler, pyparsing, dateutil, pytz, six, matplotlib-pyodide) | ~7 MB | matplotlib |

**First load ≈ 100 MB**, dominated by scipy (46) + matplotlib subtree (~23) +
numpy (12) + CPython (10).

**Target after this plan ≈ 30 MB** (CPython 9.6 + stdlib 2.2 + numpy 11.9 +
openblas 5.8 + our ~5). openblas stays (numpy needs it); scipy + the whole
matplotlib subtree go away. **~70 MB / ~70% reduction.**

Both wins rely on the same trick: register a tiny stub module in `sys.modules`
*before* `import pyrocko`, so pyrocko's top-level imports succeed without the real
package. The stubs live in **our** code (`web/bridge.py` preamble) — pyrocko stays
unmodified.

---

## Part A — drop matplotlib (~23 MB, low risk)

### Why it's loaded
`pyrocko/orthodrome.py:14` does `from matplotlib.path import Path` at module top,
and `trace.py` imports `orthodrome` at top — so importing `pyrocko.trace` drags in
all of matplotlib. `Path` is used *only* in point-in-polygon helpers
(`orthodrome.py:28,41,52` → `contains_points` / `contains_point`) which the viewer
never calls.

### Approach (stub, zero pyrocko edits)
Before importing pyrocko, inject a fake `matplotlib.path` providing a `Path` with
a numpy ray-casting `contains_point(s)` — so those orthodrome functions even keep
working:

```python
import sys, types
import numpy as np

def _point_in_poly(verts, pts):
    verts = np.asarray(verts); pts = np.atleast_2d(pts)
    x, y = pts[:, 0], pts[:, 1]
    inside = np.zeros(len(pts), bool)
    n = len(verts); j = n - 1
    for i in range(n):
        xi, yi = verts[i]; xj, yj = verts[j]
        hit = ((yi > y) != (yj > y)) & (x < (xj - xi) * (y - yi) / (yj - yi + 1e-30) + xi)
        inside ^= hit; j = i
    return inside

class _Path:
    def __init__(self, verts, closed=True, **kw): self.verts = np.asarray(verts)
    def contains_points(self, pts): return _point_in_poly(self.verts, pts)
    def contains_point(self, pt): return bool(_point_in_poly(self.verts, [pt])[0])

mpl = types.ModuleType("matplotlib")
mpath = types.ModuleType("matplotlib.path"); mpath.Path = _Path
mpl.path = mpath
sys.modules.setdefault("matplotlib", mpl)
sys.modules.setdefault("matplotlib.path", mpath)
```

Then remove `"matplotlib"` from the `loadPackage([...])` list in `web/bridge.js`
(its dep subtree — pillow/fonttools/contourpy/… — disappears automatically).

### Risks / verify
- Confirm nothing else in the trace/io import path imports `matplotlib.pyplot`
  (grep says no — only `orthodrome` uses `matplotlib.path`).
- Verify `import pyrocko.trace`, `pyrocko.io.load(mseed)`, and an `orthodrome`
  distance call still work with matplotlib absent.

---

## Part B — drop scipy (~46 MB, medium risk)

### Why it's loaded
`from scipy import signal` is a **module-top** import in `trace.py:19`,
`util.py:95`, and `response.py:16`. So `import pyrocko.trace` requires `scipy`
to exist. The functions actually executed on our hot path are only:
- `signal.butter(...)` (coeffs) — `trace.py:3516`
- `signal.lfilter(b, a, data)` / `signal.sosfilt(sos, data)` — the highpass /
  lowpass / bandpass appliers (`trace.py:1016–1190`)

Everything else (`firwin`/`remez`/`cheby1` for `downsample`; `freqs`/`freqz`/
`zpk2tf`/`cont2discrete` for response deconvolution; `fftconvolve` for
correlation) is in code paths the viewer doesn't hit. Our own waterfall uses
`scipy.signal.spectrogram` (`web/bridge.py:211`) — that must be reimplemented too.

### Approach — stub `scipy.signal`, do filtering in numpy
Register a fake `scipy` / `scipy.signal` in `sys.modules` before importing
pyrocko. Implement the few hot functions in numpy; make the rest lazy stubs that
raise `NotImplementedError` (so unused paths fail loudly only if ever reached).

Two implementation choices for the actual filtering:

**Option B1 — vendor scipy's IIR (faithful, slower).**
Port `butter` (analog prototype → bilinear → `b,a`/`sos`) and `lfilter` / `sosfilt`
from scipy (BSD-3 — vendor with license + attribution). `butter` is pure math;
`lfilter`/`sosfilt` are inherently sequential IIR recurrences, so a numpy/Python
port is O(N) per trace and slower than scipy's C loop. Mitigation: we already
`chop` to the visible window (+ filter pad) before filtering, and decimate after,
so N is bounded. Result is bit-comparable to today.

**Option B2 — zero-phase FFT-domain Butterworth (recommended, fast, pure numpy).**
Monkeypatch `pyrocko.trace.Trace.{highpass,lowpass,bandpass}` (in our bridge —
additional code, no pyrocko edit) to apply the Butterworth *magnitude* response in
the frequency domain. Fully vectorized; no `lfilter`. pyrocko itself ships an
FFT-filtering mode, so there's precedent.

```python
def _fft_butter(y, deltat, kind, fc, order=4):
    n = len(y)
    Y = np.fft.rfft(np.asarray(y, float) - np.mean(y))
    f = np.fft.rfftfreq(n, d=deltat)
    w = np.where(f == 0.0, 1e-30, f) / fc
    if kind == "low":
        H = 1.0 / np.sqrt(1.0 + w ** (2 * order))
    else:  # high
        H = 1.0 / np.sqrt(1.0 + (1.0 / w) ** (2 * order))
    return np.fft.irfft(Y * H, n)
# bandpass = high(corner_hp) then low(corner_lp)
```

Tradeoff: zero-phase (no group delay) and magnitude-only vs scipy's causal
`lfilter` — visually equivalent for inspection (arguably cleaner: no phase
distortion). Document it.

### Also reimplement the spectrogram (we use it)
Replace `scipy.signal.spectrogram` in `get_view`'s waterfall branch with a numpy
STFT:

```python
def _stft_power(y, fs, nperseg, noverlap):
    step = nperseg - noverlap
    win = np.hanning(nperseg)
    nseg = 1 + (len(y) - nperseg) // step
    cols = [np.fft.rfft(y[i*step:i*step+nperseg] * win) for i in range(nseg)]
    S = np.abs(np.array(cols).T) ** 2          # [freq][time]
    f = np.fft.rfftfreq(nperseg, d=1.0/fs)
    return f, S
```

### Then
Remove `"scipy"` from `loadPackage([...])` in `web/bridge.js`. (openblas stays —
numpy needs it.)

### Risks / verify
- Filter shape: compare FFT-Butterworth output vs current scipy output on the
  synthetic example + `test.mseed` (peak-to-peak ratios, visual). Expect close,
  not identical (zero-phase vs causal).
- Audit for any *other* scipy submodule used in modules we import
  (`scipy.fft`, `scipy.interpolate`, `scipy.linalg`) — provide stubs as needed.
- Keep the `NotImplementedError` stubs informative so a future feature that needs
  real scipy (e.g. response deconvolution, `downsample(ftype='fir-remez')`) fails
  with a clear "scipy stubbed out — see plan/load-time-reduction.md" message.
- Decide policy: if deconvolution is ever wired up for real, it needs
  `freqs`/`zpk2tf` etc. — either implement those in the stub or re-add scipy
  behind a feature flag / lazy `micropip.install("scipy")` on demand.

---

## Combined rollout

1. Part A first (low risk, big-ish win, isolated).
2. Then Part B Option B2 (FFT filtering) + numpy STFT.
3. Update the loading-screen stage labels (no more "matplotlib").
4. Re-measure first-load; expect ~100 MB → ~30 MB.
5. Update `README.md` notes + `[[snuffler-web-toolchain]]` memory.

## Fallback / nice-to-have
- If on-demand richness is ever wanted, lazy-`micropip.install("scipy")` only when
  a scipy-only feature (deconvolution) is invoked, keeping the default load light.
