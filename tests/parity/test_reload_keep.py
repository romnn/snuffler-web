"""Reload keep-list enforcement.

Shift+R reloads bridge.py and restores the session state named by its
``_RELOAD_KEEP`` tuple.
A name in that tuple that no longer exists as a module-level global silently
drops that state on every reload — exactly how a TypeScript copy of the list
once lost the instrument responses (it still said ``_STATIONXML`` after the
global became ``_STATIONXMLS``).
The list is name-keyed, not versioned: renaming a state global deliberately
resets that state on the next reload (``_reload_snapshot`` detaches the names
from the module dict, ``_reload_restore`` reinstalls only what the new code
still claims), so this check pins only that the *current* list matches the
*current* globals.
Parsed from source rather than imported, because bridge.py needs
pyrocko/pyodide to import and this check must run in the dependency-free unit
environment.

    task test:parity:unit
"""

from __future__ import annotations

import ast
from pathlib import Path
from typing import Any, Dict, Set, Tuple

_WEB = Path(__file__).resolve().parents[2] / "web"


def _module_globals_and_keep() -> Tuple[Set[str], Tuple[str, ...]]:
    tree = ast.parse((_WEB / "bridge.py").read_text())
    names: Set[str] = set()
    keep: Tuple[str, ...] = ()
    for node in tree.body:
        if isinstance(node, ast.Assign):
            targets, value = node.targets, node.value
        elif isinstance(node, ast.AnnAssign) and node.value is not None:
            targets, value = [node.target], node.value
        else:
            continue
        for target in targets:
            if isinstance(target, ast.Name):
                names.add(target.id)
                if target.id == "_RELOAD_KEEP":
                    keep = tuple(ast.literal_eval(value))
    assert keep, "bridge.py no longer defines _RELOAD_KEEP"
    return names, keep


def test_every_kept_name_is_a_module_global() -> None:
    # A keep-list name with no module-level assignment is a rename that missed
    # the list: the successor global never enters the snapshot, so that state
    # dies on the next reload — while the snapshot dutifully carries the
    # detached old name that nothing reads anymore.
    names, keep = _module_globals_and_keep()
    missing = [name for name in keep if name not in names]
    assert not missing, "_RELOAD_KEEP names without a module-level assignment: %s" % missing
    assert len(keep) == len(set(keep)), "duplicate names in _RELOAD_KEEP"


def test_the_worker_defers_to_the_python_owned_list() -> None:
    # The worker must call the snapshot/restore pair instead of keeping its own
    # copy of the names, which is how `_STATIONXML` went stale in the first
    # place.
    src = (_WEB / "bridge_worker.ts").read_text()
    assert "_reload_snapshot" in src
    assert "_reload_restore" in src
    assert "_keep" not in src, "the worker grew its own keep list again"
    # The snapshot detaches state from the module dict, so a reload that raises
    # would strand the session unless restore runs unconditionally.
    assert "finally:" in src, "the worker must restore the snapshot in a finally"


def _reload_machinery() -> Dict[str, Any]:
    """The real `_reload_snapshot`/`_reload_restore` bodies from bridge.py,
    executed into a bare namespace standing in for the module dict — so their
    behaviour is tested without importing bridge.py (which needs pyrocko)."""
    tree = ast.parse((_WEB / "bridge.py").read_text())
    wanted = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef)
        and node.name in ("_reload_snapshot", "_reload_restore")
    ]
    assert len(wanted) == 2, "bridge.py no longer defines the snapshot/restore pair"
    # The signatures reference typing names and restore bumps the generation
    # counter; stub exactly that surface.
    namespace: Dict[str, Any] = {"Dict": dict, "Any": object, "_mark_traces_changed": lambda: None}
    exec(  # noqa: S102 — executing our own parsed source, not external input
        compile(ast.Module(body=wanted, type_ignores=[]), "<bridge-reload>", "exec"),
        namespace,
    )
    return namespace


def test_a_reload_round_trip_detaches_renamed_state() -> None:
    # The old module version: one global that survives into the new code and
    # one the new source no longer defines.
    mod = _reload_machinery()
    mod["_RELOAD_KEEP"] = ("_KEEP_ME", "_RENAMED_AWAY")
    mod["_KEEP_ME"] = ["session"]
    mod["_RENAMED_AWAY"] = ["stale"]
    saved = mod["_reload_snapshot"]()
    # The snapshot must detach: `importlib.reload` reuses the module dict, so
    # any name left here would survive the reload as a zombie global.
    assert "_KEEP_ME" not in mod
    assert "_RENAMED_AWAY" not in mod
    # `importlib.reload` re-executes the new source over the same dict: the
    # living global comes back with its fresh default, under the new keep-list.
    mod["_KEEP_ME"] = []
    mod["_RELOAD_KEEP"] = ("_KEEP_ME",)
    mod["_reload_restore"](saved)
    # Session state survived the rename boundary; the retired name stayed dead
    # instead of shadowing its successor.
    assert mod["_KEEP_ME"] == ["session"]
    assert "_RENAMED_AWAY" not in mod
