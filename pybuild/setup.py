"""Configurable build of pyrocko for Pyodide (emscripten).

This REPLACES pyrocko's own build system for the web target. Which C extensions
get compiled is selected by the ``PYROCKO_EXTS`` environment variable; the
default (``full``) builds every extension that cross-compiles cleanly — i.e. all
of them except ``evalresp_ext`` (which needs the external ``evalresp`` C library,
``<evresp.h>``, that is not bundled).

  PYROCKO_EXTS = full     (default) — all buildable extensions, incl. pyrocko.gf
                                      (store/eikonal/okada → synthetics + cake GF
                                      stores) and parstack/ahfullgreen/etc.
                 gf                 — just what `import pyrocko.gf` + GF stores need
                 minimal            — only the waveform-inspector core
                 a,b,c              — explicit comma-separated list of ext keys

  PYROCKO_OPENMP = 0      (default) — serial build, loads into any Pyodide.
                   1                — pass -fopenmp, as docker/pyodide-mt does.
                                      OpenMP maps onto wasm pthreads, so the wheel
                                      only loads into a pthread-enabled Pyodide on
                                      a cross-origin-isolated page.

The pure-Python pyrocko tree is shipped unmodified (staged next to this file by
docker/pyodide-mt/build-pyrocko.sh, which also extracts and patches libmseed).
"""

import os
import os.path as op

import numpy
from setuptools import Extension, find_packages, setup

NUMPY_INC = numpy.get_include()

# OpenMP off by default (see module docstring). When on, Emscripten compiles
# OpenMP to wasm pthreads (needs -pthread + a cross-origin-isolated, pthread
# Pyodide); when off, `#pragma omp` is ignored and the guarded `omp_*` calls
# compile out, so the extensions run correctly, just serially.
_OPENMP = os.environ.get("PYROCKO_OPENMP", "0") == "1"
_OMP_ARGS = ["-fopenmp"] if _OPENMP else []

libmseed_sources = [
    op.join("libmseed", f)
    for f in [
        "fileutils.c",
        "genutils.c",
        "gswap.c",
        "lmplatform.c",
        "logging.c",
        "lookup.c",
        "msrutils.c",
        "pack.c",
        "packdata.c",
        "parseutils.c",
        "selection.c",
        "tracelist.c",
        "traceutils.c",
        "unpack.c",
        "unpackdata.c",
    ]
]


def _ext(name, sources, include_dirs=(), extra_compile_args=(), use_omp=False):
    omp = _OMP_ARGS if use_omp else []
    return Extension(
        name,
        sources=list(sources),
        include_dirs=[NUMPY_INC, *include_dirs],
        extra_compile_args=[*extra_compile_args, *omp],
        extra_link_args=[*omp],
    )


def _p(*parts):
    return op.join("pyrocko", *parts)


# All buildable extensions, keyed by a short name used by PYROCKO_EXTS. The
# module path (the Extension name) matches how pyrocko imports each one — note
# the io extensions import as top-level `pyrocko.<name>` (like mseed_ext), not
# `pyrocko.io.<name>`.
ALL_EXTS = {
    # pyrocko.pile's ordered trace index (the vendored pyavl). `pile.py` imports
    # it at module top, so a wheel without it cannot import the pile at all —
    # and the bridge serves every view from a pile, so a runtime built without
    # this one cannot open a file.
    "avl": _ext(
        "pyrocko.avl",
        [
            _p("ext", "pyavl-1.12", "avl.c"),
            _p("ext", "pyavl-1.12", "avlmodule.c"),
        ],
        # The macros pyrocko's own setup.py defines for this extension, plus its
        # warning suppressions for the 2005-era C.
        extra_compile_args=[
            "-DHAVE_AVL_VERIFY",
            "-DAVL_FOR_PYTHON",
            "-Wno-parentheses",
            "-Wno-uninitialized",
        ],
    ),
    "signal_ext": _ext("pyrocko.signal_ext", [_p("ext", "signal_ext.c")]),
    "mseed_ext": _ext(
        "pyrocko.mseed_ext",
        [_p("io", "ext", "mseed_ext.c"), *libmseed_sources],
        include_dirs=["libmseed"],
        extra_compile_args=["-fno-strict-aliasing"],
    ),
    "eikonal_ext": _ext("pyrocko.eikonal_ext", [_p("ext", "eikonal_ext.c")], use_omp=True),
    "orthodrome_ext": _ext("pyrocko.orthodrome_ext", [_p("ext", "orthodrome_ext.c")]),
    "parstack_ext": _ext("pyrocko.parstack_ext", [_p("ext", "parstack_ext.c")], use_omp=True),
    "util_ext": _ext("pyrocko.util_ext", [_p("ext", "util_ext.c")]),
    "autopick_ext": _ext("pyrocko.autopick_ext", [_p("ext", "autopick_ext.c")]),
    "ahfullgreen_ext": _ext("pyrocko.ahfullgreen_ext", [_p("ext", "ahfullgreen_ext.c")]),
    # GF stores + synthetics. store_ext bundles its lookup-table helper (lut.c)
    # and #include "lut.h" from the same dir.
    "store_ext": _ext(
        "pyrocko.gf.store_ext",
        [_p("gf", "ext", "store_ext.c"), _p("gf", "ext", "lut.c")],
        include_dirs=[_p("gf", "ext")],
        use_omp=True,
    ),
    "okada_ext": _ext(
        "pyrocko.modelling.okada_ext",
        [_p("modelling", "ext", "okada_ext.c")],
        use_omp=True,
    ),
    "disloc_ext": _ext("pyrocko.modelling.disloc_ext", [_p("modelling", "ext", "disloc_ext.c")]),
    "datacube_ext": _ext("pyrocko.datacube_ext", [_p("io", "ext", "datacube_ext.c")]),
    "ims_ext": _ext("pyrocko.ims_ext", [_p("io", "ext", "ims_ext.c")]),
}

# Extension sets selectable via PYROCKO_EXTS.
_CORE = ["signal_ext", "mseed_ext", "avl"]
_GF = _CORE + ["eikonal_ext", "okada_ext", "store_ext", "orthodrome_ext"]
_GROUPS = {
    "minimal": _CORE,
    "gf": _GF,
    "full": list(ALL_EXTS),
}

_sel = os.environ.get("PYROCKO_EXTS", "full").strip()
if _sel in _GROUPS:
    _names = _GROUPS[_sel]
else:
    _names = [s.strip() for s in _sel.split(",") if s.strip()]

_unknown = [n for n in _names if n not in ALL_EXTS]
if _unknown:
    raise SystemExit("PYROCKO_EXTS: unknown extension(s): %s" % ", ".join(_unknown))

ext_modules = [ALL_EXTS[n] for n in _names]
print(
    "pybuild: PYROCKO_EXTS=%s OPENMP=%s -> building %d extension(s): %s"
    % (_sel, int(_OPENMP), len(ext_modules), ", ".join(_names))
)

setup(
    name="pyrocko",
    version="2026.06.02",
    packages=find_packages(include=["pyrocko", "pyrocko.*"]),
    ext_modules=ext_modules,
    zip_safe=False,
)
