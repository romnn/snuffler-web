"""The Python half of the worker's boot: what to defer, and what to report.

``web/bridge_worker.ts`` imports this module before the bridge, keeping two
boot-time concerns out of the ``snuffler_bridge`` package: which third-party
imports are deferred, and how import progress reaches the loading bar.
Deferring changes only when pyrocko's own dependencies execute, never pyrocko
itself.
"""

from __future__ import annotations

import importlib.util
import sys
from types import ModuleType
from typing import Callable, List, Mapping, Optional, Sequence

# scipy imports pyrocko performs at module scope but never uses while the module
# body runs — every reference is inside a function, verified against the pinned
# pyrocko source:
#
#   pyrocko.util, pyrocko.trace, pyrocko.response   from scipy import signal
#   pyrocko.cake                                    from scipy.optimize import ...
#
# Importing scipy.signal costs ~3.5 s in this wasm build (it pulls in scipy.fft,
# .linalg, .sparse and .interpolate), all of it on the page-load critical path for
# code the first waveform view never calls.
#
# matplotlib is deliberately NOT here: pyrocko.orthodrome builds a Path at module
# scope to probe for `contains_points`, so it genuinely needs matplotlib while its
# body runs and deferring it would only move the cost by a few statements.
DEFERRED = ("scipy.signal", "scipy.optimize")


def defer(names: Sequence[str] = DEFERRED) -> List[str]:
    """Pre-seed ``sys.modules`` with lazy stand-ins for `names`.

    A later ``from scipy import signal`` then binds a module object that executes
    the real module on first attribute access.
    That access is synchronous on the interpreter's one Python thread, so no
    caller can observe a half-initialised module.

    Returns the names actually deferred; a module already imported, or one whose
    loader does not support lazy execution, is skipped.
    """
    deferred: List[str] = []
    for name in names:
        if name in sys.modules:
            continue
        try:
            spec = importlib.util.find_spec(name)
        except (ImportError, ValueError):
            continue
        if spec is None or spec.loader is None:
            continue
        spec.loader = importlib.util.LazyLoader(spec.loader)
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
        # `from scipy import signal` reads the attribute off the parent package
        # first, so bind it there too rather than relying on the import system's
        # sys.modules fallback.
        parent, _, attribute = name.rpartition(".")
        if parent in sys.modules:
            setattr(sys.modules[parent], attribute, module)
        deferred.append(name)
    return deferred


def warm(names: Sequence[str] = DEFERRED) -> List[str]:
    """Execute the deferred modules now, so the first real use does not pay for it.

    Called once the app is interactive.
    Touching any attribute is what forces a lazy module to execute; ``__name__``
    is enough and is defined on every module.
    """
    warmed: List[str] = []
    for name in names:
        module = sys.modules.get(name)
        if module is None:
            continue
        _ = module.__name__
        warmed.append(name)
    return warmed


def import_bridge(progress: Callable[[int], None]) -> ModuleType:
    """Import the bridge (and with it pyrocko), reporting modules as they load.

    Returns ``snuffler_bridge.service``, whose ``call`` the worker dispatches every
    rpc through.

    The import is by far the longest stretch of the boot with nothing to show for
    it, so `progress` is called with the running count of modules imported.
    Counting in ``__import__`` rather than a ``sys.meta_path`` finder keeps cached
    imports (the vast majority of calls) to one dict lookup.
    """
    import builtins

    count = 0
    original = builtins.__import__

    # Mirrors builtins.__import__ exactly, parameter names included: the interpreter
    # calls it positionally, but library code that calls it by keyword must keep
    # working while this is installed.
    def counting(
        name: str,
        globals: Optional[Mapping[str, object]] = None,
        locals: Optional[Mapping[str, object]] = None,
        fromlist: Sequence[str] = (),
        level: int = 0,
    ) -> ModuleType:
        nonlocal count
        if name not in sys.modules:
            count += 1
            # Reporting every module would post more messages than the loading bar
            # can use; every 8th keeps it visibly moving for a fraction of the cost.
            if count % 8 == 0:
                progress(count)
        return original(name, globals, locals, fromlist, level)

    builtins.__import__ = counting
    try:
        module = importlib.import_module("snuffler_bridge.service")
    finally:
        builtins.__import__ = original
    progress(count)
    return module
