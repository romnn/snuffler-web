"""Minimal build of pyrocko for Pyodide (emscripten).

This REPLACES pyrocko's own build system for the web target. Instead of
compiling all 14 C extensions (several of which pull in OpenMP / evalresp /
external C libs that don't cross-compile cleanly), we build only what the
waveform inspector needs:

  * pyrocko.signal_ext  — required to `import pyrocko.trace` (numpy only)
  * pyrocko.mseed_ext   — required to load MiniSEED (mseed_ext.c + libmseed)

The pure-Python pyrocko tree is shipped unmodified (copied into ./pyrocko by
build-wheel.sh). libmseed is extracted + patched there too.
"""

import os.path as op

import numpy
from setuptools import Extension, find_packages, setup

libmseed_sources = [
    op.join("libmseed", f)
    for f in [
        "fileutils.c", "genutils.c", "gswap.c", "lmplatform.c", "logging.c",
        "lookup.c", "msrutils.c", "pack.c", "packdata.c", "parseutils.c",
        "selection.c", "tracelist.c", "traceutils.c", "unpack.c",
        "unpackdata.c",
    ]
]

ext_modules = [
    Extension(
        "pyrocko.signal_ext",
        sources=[op.join("pyrocko", "ext", "signal_ext.c")],
        include_dirs=[numpy.get_include()],
    ),
    Extension(
        "pyrocko.mseed_ext",
        sources=[op.join("pyrocko", "io", "ext", "mseed_ext.c")] + libmseed_sources,
        include_dirs=[numpy.get_include(), "libmseed"],
        extra_compile_args=["-fno-strict-aliasing"],
    ),
]

setup(
    name="pyrocko",
    version="2026.06.02",
    packages=find_packages(include=["pyrocko", "pyrocko.*"]),
    ext_modules=ext_modules,
    zip_safe=False,
)
