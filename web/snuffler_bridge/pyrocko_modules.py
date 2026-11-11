"""pyrocko modules loaded without running their package's heavy ``__init__``."""

from __future__ import annotations

import os
from typing import Any, Tuple

# Each module is loaded once and kept here.
_marker: Any = None
_gf_store: Any = None
_gf_meta: Any = None


def marker_module() -> Any:
    """Load pyrocko's ``marker`` module without running the snuffler package init.

    ``pyrocko/gui/snuffler/__init__.py`` imports the Qt application, and PyQt
    cannot exist in Pyodide.
    marker.py itself only needs util/plot/model/trace, so a stub ``snuffler``
    package carrying the real directory on ``__path__`` stands in for it.
    """
    global _marker
    if _marker is None:
        import importlib
        import sys
        import types

        import pyrocko.gui

        pkg_name = "pyrocko.gui.snuffler"
        existing = sys.modules.get(pkg_name)
        if existing is None or not getattr(existing, "_snuffler_stub", False):
            stub = types.ModuleType(pkg_name)
            stub.__path__ = [os.path.join(os.path.dirname(pyrocko.gui.__file__), "snuffler")]
            stub.__package__ = pkg_name
            setattr(stub, "_snuffler_stub", True)
            sys.modules[pkg_name] = stub
        _marker = importlib.import_module(pkg_name + ".marker")
    return _marker


def gf_store_modules() -> Tuple[Any, Any]:
    """Load ``pyrocko.gf.store`` and ``pyrocko.gf.meta`` without running
    ``pyrocko/gf/__init__.py``, the way `marker_module` loads the marker module.

    ``import pyrocko.gf`` pulls in ``requests`` through RemoteEngine, which reading
    a local store's config and earth model does not need.
    """
    global _gf_store, _gf_meta
    if _gf_store is None or _gf_meta is None:
        import importlib
        import sys
        import types

        import pyrocko

        pkg = "pyrocko.gf"
        existing = sys.modules.get(pkg)
        if existing is None or not getattr(existing, "_gf_stub", False):
            stub = types.ModuleType(pkg)
            stub.__path__ = [os.path.join(os.path.dirname(pyrocko.__file__), "gf")]
            stub.__package__ = pkg
            setattr(stub, "_gf_stub", True)
            sys.modules[pkg] = stub
        _gf_meta = importlib.import_module(pkg + ".meta")
        _gf_store = importlib.import_module(pkg + ".store")
    return _gf_store, _gf_meta


def import_full_gf() -> Any:
    """Import the full ``pyrocko.gf``.

    First evicts the ``gf.store``/``gf.meta`` stub that `gf_store_modules`
    installs for the cake GF-store path.
    """
    import importlib
    import sys

    existing = sys.modules.get("pyrocko.gf")
    if existing is not None and getattr(existing, "_gf_stub", False):
        for key in [k for k in sys.modules if k == "pyrocko.gf" or k.startswith("pyrocko.gf.")]:
            del sys.modules[key]
    return importlib.import_module("pyrocko.gf")
