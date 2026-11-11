"""Reload keep-list enforcement.

Shift+R reloads the ``snuffler_bridge`` package and restores the session state named by
the ``RELOAD_KEEP`` tuple in its ``state`` module.
A name in that tuple that does not exist as a module-level global silently drops that
state on every reload.
The list is name-keyed, not versioned: renaming a state global deliberately resets that
state on the next reload (``reload_snapshot`` detaches the names from the module dict,
``reload_restore`` reinstalls only what the new code still claims), so this check pins
only that the *current* list matches the *current* globals.
Parsed from source rather than imported, because the bridge needs pyrocko/pyodide to
import and this check must run in the dependency-free unit environment.

    task test:parity:unit
"""

from __future__ import annotations

import ast
import re
from pathlib import Path
from typing import Any, Dict, Set, Tuple

_WEB = Path(__file__).resolve().parents[2] / "web"
_PACKAGE = _WEB / "snuffler_bridge"
_STATE = _PACKAGE / "state.py"


def _module_globals_and_keep() -> Tuple[Set[str], Tuple[str, ...]]:
    tree = ast.parse(_STATE.read_text())
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
                if target.id == "RELOAD_KEEP":
                    keep = tuple(ast.literal_eval(value))
    assert keep, "state.py no longer defines RELOAD_KEEP"
    return names, keep


def test_every_kept_name_is_a_module_global() -> None:
    # A keep-list name with no module-level assignment is a rename that missed
    # the list: the successor global never enters the snapshot, so that state
    # dies on the next reload — while the snapshot dutifully carries the
    # detached old name that nothing reads anymore.
    names, keep = _module_globals_and_keep()
    missing = [name for name in keep if name not in names]
    assert not missing, "RELOAD_KEEP names without a module-level assignment: %s" % missing
    assert len(keep) == len(set(keep)), "duplicate names in RELOAD_KEEP"


def test_the_worker_defers_to_the_python_owned_list() -> None:
    # The worker must call the snapshot/restore pair instead of keeping its own
    # copy of the names, which would go stale on the next rename.
    src = (_WEB / "bridge_worker.ts").read_text()
    assert "reload_snapshot" in src
    assert "reload_restore" in src
    assert "_keep" not in src, "the worker grew its own keep list again"
    # The snapshot detaches state from the module dict, so a reload that raises
    # would strand the session unless restore runs unconditionally.
    assert "finally:" in src, "the worker must restore the snapshot in a finally"


def test_the_worker_stages_every_bridge_module() -> None:
    # A static file server cannot list a directory, so the worker names every
    # module it fetches; a module missing there never reaches Pyodide.
    src = (_WEB / "bridge_worker.ts").read_text()
    listing = re.search(r"const BRIDGE_PACKAGE_MODULES = \[(.*?)\];", src, re.S)
    assert listing, "the worker no longer declares BRIDGE_PACKAGE_MODULES"
    staged = re.findall(r'"(\w+)"', listing.group(1))
    on_disk = sorted(path.stem for path in _PACKAGE.glob("*.py"))
    assert sorted(staged) == on_disk
    assert len(staged) == len(set(staged)), "duplicate modules in BRIDGE_PACKAGE_MODULES"
    # Dependency order: a reload then re-executes each module after the ones it
    # imports.
    for position, module in enumerate(staged):
        tree = ast.parse((_PACKAGE / f"{module}.py").read_text())
        imported = {
            alias.name if node.module is None else node.module
            for node in ast.walk(tree)
            if isinstance(node, ast.ImportFrom) and node.level == 1
            for alias in node.names
        }
        late = sorted(name for name in imported if staged.index(name) > position)
        assert not late, f"{module} is staged before the modules it imports: {late}"


def _reload_machinery() -> Dict[str, Any]:
    """The real `reload_snapshot`/`reload_restore` bodies from state.py,
    executed into a bare namespace standing in for the module dict — so their
    behaviour is tested without importing the bridge (which needs pyrocko)."""
    tree = ast.parse(_STATE.read_text())
    wanted = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name in ("reload_snapshot", "reload_restore")
    ]
    assert len(wanted) == 2, "state.py no longer defines the snapshot/restore pair"
    # The signatures reference typing names and restore bumps the generation
    # counter; stub exactly that surface.
    namespace: Dict[str, Any] = {"Dict": dict, "Any": object, "mark_traces_changed": lambda: None}
    exec(  # noqa: S102 — executing our own parsed source, not external input
        compile(ast.Module(body=wanted, type_ignores=[]), "<bridge-reload>", "exec"),
        namespace,
    )
    return namespace


def test_a_reload_round_trip_detaches_renamed_state() -> None:
    # The old module version: one global that survives into the new code and
    # one the new source no longer defines.
    mod = _reload_machinery()
    mod["RELOAD_KEEP"] = ("_KEEP_ME", "_RENAMED_AWAY")
    mod["_KEEP_ME"] = ["session"]
    mod["_RENAMED_AWAY"] = ["stale"]
    saved = mod["reload_snapshot"]()
    # The snapshot must detach: `importlib.reload` reuses the module dict, so
    # any name left here would survive the reload as a zombie global.
    assert "_KEEP_ME" not in mod
    assert "_RENAMED_AWAY" not in mod
    # `importlib.reload` re-executes the new source over the same dict: the
    # living global comes back with its fresh default, under the new keep-list.
    mod["_KEEP_ME"] = []
    mod["RELOAD_KEEP"] = ("_KEEP_ME",)
    mod["reload_restore"](saved)
    # Session state survived the rename boundary; the retired name stayed dead
    # instead of shadowing its successor.
    assert mod["_KEEP_ME"] == ["session"]
    assert "_RENAMED_AWAY" not in mod
