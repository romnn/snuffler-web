"""Tier 2: extract Qt snuffler's whole command surface from the live widget tree.

Extraction rather than a checklist, because a checklist is stale the moment upstream
adds a menu item and nobody notices. Everything here is read off a real, offscreen
``PileViewer``: the menu bar walked recursively, the built-in snufflings' declared
parameters, and the key table snuffler ships as its own help page.

Writes JSON on stdout, or to ``--out``. :mod:`run_inventory` diffs it against the
web inventory.
"""

from __future__ import annotations

# harness first: it prepares the environment before pyrocko and Qt load.
import harness  # noqa: F401 — imported for its side effects  # isort: skip

import argparse
import html.parser
import json
import re
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, Iterator, List, Tuple

import pyrocko.util
import qt_oracle
from corpus import build as corpus_build
from PyQt5 import QtWidgets as qw


def _kind(action: qw.QAction) -> str:
    if action.menu() is not None:
        return "menu"
    if not action.isCheckable():
        return "action"
    group = action.actionGroup()
    return "radio" if group is not None and group.isExclusive() else "check"


def walk_menu(menu: qw.QWidget, path: Tuple[str, ...] = ()) -> Iterator[Dict[str, Any]]:
    for action in menu.actions():
        if action.isSeparator():
            continue
        text = qt_oracle.clean_menu_text(action.text())
        submenu = action.menu()
        if not text:
            # Widget actions: the toolbar the menu bar carries is a layout, not a
            # command, and has nothing to compare against.
            continue
        here = path + (text,)
        entry = {
            "path": "/".join(here),
            "text": text,
            "kind": _kind(action),
            "checkable": bool(action.isCheckable()),
            "checked": bool(action.isChecked()),
            "enabled": bool(action.isEnabled()),
            "shortcut": action.shortcut().toString(),
        }
        yield entry
        if submenu is not None:
            yield from walk_menu(submenu, here)


def parameter_entry(param: Any) -> Dict[str, Any]:
    """Flatten one Param / Switch / Choice declaration."""
    entry: Dict[str, Any] = {
        "name": getattr(param, "name", ""),
        "ident": getattr(param, "ident", ""),
        "default": getattr(param, "default", None),
    }
    if hasattr(param, "choices"):
        entry["type"] = "choice"
        entry["choices"] = list(param.choices)
    elif hasattr(param, "minimum"):
        entry["type"] = "param"
        entry["minimum"] = param.minimum
        entry["maximum"] = param.maximum
    else:
        entry["type"] = "switch"
    return entry


def snufflings(view: Any) -> List[Dict[str, Any]]:
    """Every built-in snuffling with the knobs it declares.

    ``snuffling_paths`` is redirected first: ``iter_snuffling_modules`` creates the
    directory it is pointed at and imports every ``.py`` in it, and a test harness
    has no business loading — or creating — whatever is in the developer's
    ``~/.snufflings``.
    """
    with tempfile.TemporaryDirectory(prefix="parity-snufflings-") as scratch:
        view.snuffling_paths = [scratch]
        view.setup_snufflings()

    out = []
    for snuffling in view.snufflings:
        out.append(
            {
                "name": snuffling.get_name(),
                "parameters": [parameter_entry(p) for p in snuffling.get_parameters()],
                "triggers": [name for name, _method in getattr(snuffling, "_triggers", [])],
            }
        )
    return sorted(out, key=lambda item: item["name"])


class _HelpTableParser(html.parser.HTMLParser):
    """Pull snuffler's shipped help tables apart into ``{section: rows}``.

    Each table opens with a ``<th>`` naming its section — Key, Mouse, Command — and
    the rows under it are (shortcut, description) pairs.
    """

    def __init__(self) -> None:
        super().__init__()
        self.sections: Dict[str, List[Dict[str, str]]] = {}
        self._section = ""
        self._cells: List[str] = []
        self._buffer: List[str] = []
        self._in_cell = False
        self._is_header = False

    def handle_starttag(self, tag: str, attrs: Any) -> None:
        if tag == "tr":
            self._cells = []
            self._is_header = False
        elif tag in ("td", "th"):
            self._in_cell = True
            self._is_header = self._is_header or tag == "th"
            self._buffer = []

    def handle_endtag(self, tag: str) -> None:
        if tag in ("td", "th"):
            self._in_cell = False
            self._cells.append(re.sub(r"\s+", " ", "".join(self._buffer)).strip())
        elif tag == "tr":
            if not self._cells or not self._cells[0]:
                return
            if self._is_header:
                self._section = self._cells[0]
                self.sections.setdefault(self._section, [])
            elif len(self._cells) >= 2 and self._section:
                self.sections[self._section].append(
                    {"key": self._cells[0], "description": self._cells[1]}
                )

    def handle_data(self, data: str) -> None:
        if self._in_cell:
            self._buffer.append(data)


# Everything worth asking the viewer about, in the harness's key notation.
def probe_tokens() -> List[str]:
    tokens = [chr(code) for code in range(ord("a"), ord("z") + 1)]
    tokens += [letter.upper() for letter in tokens]
    tokens += list("0123456789")
    tokens += [":", "?", "+", "-", "=", "_", "/"]
    tokens += [f"<f{index}>" for index in range(1, 13)]
    tokens += [
        "<space>",
        "<backspace>",
        "<delete>",
        "<escape>",
        "<tab>",
        "<enter>",
        "<pageup>",
        "<pagedown>",
        "<up>",
        "<down>",
        "<left>",
        "<right>",
        "<shift> + <tab>",
        "<shift> + <up>",
        "<shift> + <down>",
        "<shift> + <left>",
        "<shift> + <right>",
    ]
    return tokens


def key_bindings(view: Any, menu: qw.QWidget) -> List[Dict[str, Any]]:
    """Which keys snuffler actually binds, asked of the running widget.

    Two sources, because snuffler has two. Menu items carry ``QAction`` shortcuts,
    which the menu walk already sees. Everything else is a chain of comparisons inside
    ``keyPressEvent`` ending in ``else: super().keyPressEvent(key_event)`` — so the
    question "did snuffler bind this key" is exactly "did that fall-through run".
    Standing in for ``QWidget.keyPressEvent`` during the probe answers it precisely,
    where ``event.isAccepted()`` does not: PyQt leaves the event accepted either way.

    This is the live binding surface rather than a transcription of the help page,
    which is the point — it is how the harness noticed the help page understating its
    own marker-kind and phase-key ranges.

    A branch that raises counts as bound: the exception is proof the key reached a
    handler. Several of them reach for the surrounding ``SnufflerWindow`` (the marker
    editor, the main controls), which a bare ``PileViewer`` does not have.
    """
    bindings: Dict[str, Dict[str, Any]] = {}
    fell_through = False

    def record_fall_through(self: Any, event: Any) -> None:
        nonlocal fell_through
        fell_through = True

    original = qw.QWidget.keyPressEvent
    qw.QWidget.keyPressEvent = record_fall_through  # type: ignore[method-assign]
    try:
        for token in probe_tokens():
            fell_through = False
            event = qt_oracle.key_event(token)
            try:
                view.keyPressEvent(event)
                handled = not fell_through
            except Exception:  # noqa: BLE001 — reaching a handler at all is the answer
                handled = True
            if handled:
                bindings[token] = {"key": token, "source": "keyPressEvent"}
    finally:
        qw.QWidget.keyPressEvent = original  # type: ignore[method-assign]

    for action in _iter_actions(menu):
        for sequence in action.shortcuts():
            token = shortcut_token(sequence.toString())
            if token:
                bindings.setdefault(
                    token, {"key": token, "source": "shortcut", "action": action.text()}
                )

    return sorted(bindings.values(), key=lambda item: item["key"])


def _iter_actions(menu: qw.QWidget) -> Iterator[qw.QAction]:
    for action in menu.actions():
        yield action
        submenu = action.menu()
        if submenu is not None:
            yield from _iter_actions(submenu)


def shortcut_token(text: str) -> str:
    """Turn a ``QKeySequence`` string into the notation the help tables use."""
    if not text:
        return ""
    parts = [part.strip() for part in text.split(",")]
    rendered = []
    for part in parts:
        pieces = part.split("+")
        modifiers = [piece.lower() for piece in pieces[:-1]]
        base = pieces[-1]
        # A bare letter shortcut is the unshifted key, which the help pages write
        # lowercase; `A` there means shift+a and is a different command.
        base = base.lower() if len(base) == 1 else f"<{base.lower()}>"
        rendered.append(" + ".join([f"<{m}>" for m in modifiers] + [base]))
    return ", ".join(rendered)


def help_tables() -> Dict[str, List[Dict[str, str]]]:
    """The Key / Mouse / Command reference snuffler itself shows under Help.

    The live key map lives inside ``keyPressEvent`` as a chain of comparisons, so it
    cannot be read off the widget tree. The help page is upstream's own statement of
    the same thing, it ships in the same pinned source tree, and it is what the web
    port's help window was written against — so it is the honest thing to diff.
    """
    sections: Dict[str, List[Dict[str, str]]] = {}
    for name in ("snuffler_help.html", "snuffler_help_epilog.html"):
        parser = _HelpTableParser()
        parser.feed(Path(pyrocko.util.data_file(name)).read_text())
        for section, rows in parser.sections.items():
            sections.setdefault(section, []).extend(rows)
    return sections


def controls(frame: Any) -> Tuple[List[Dict[str, Any]], Any]:
    """The bottom control strip, walked rather than listed.

    ``PileViewer.controls()`` builds the strip on demand — the window is what calls
    it (``snuffler_app.py:690``), so the harness has to as well. Its widgets carry
    either a text label or, for the icon-only colorbar toggles, a tooltip, which is
    all a user has to go on either.
    """
    strip = frame.controls()
    out: List[Dict[str, Any]] = []
    seen = set()
    for widget in strip.findChildren(qw.QWidget):
        text = qt_oracle.clean_menu_text(widget.text()) if hasattr(widget, "text") else ""
        name = text or widget.toolTip().strip()
        if not name or name in seen:
            continue
        kind = type(widget).__name__
        if kind not in ("QLabel", "QPushButton", "QCheckBox", "QComboBox", "QToolButton"):
            continue
        seen.add(name)
        entry: Dict[str, Any] = {"name": name, "widget": kind}
        if hasattr(widget, "isCheckable") and widget.isCheckable():
            entry["checkable"] = True
            entry["checked"] = bool(widget.isChecked())
        if kind == "QComboBox":
            entry["choices"] = [
                widget.itemData(index) or widget.itemText(index) for index in range(widget.count())
            ]
        out.append(entry)
    return sorted(out, key=lambda item: item["name"]), strip


def collect(dataset: str = "demo", screenshots: Path | None = None) -> Dict[str, Any]:
    from scenario import DATASETS

    viewer = qt_oracle.QtViewer([corpus_build.path(DATASETS[dataset].waveforms)])
    host = None
    try:
        # Snufflings first: each one adds itself to the Snufflings menu as it is
        # loaded, so walking the menu before they exist would miss all of them.
        loaded = snufflings(viewer.view)
        control_entries, control_strip = controls(viewer.frame)
        # Probing keys runs real handlers, several of which reach for the window the
        # viewer is in, so give it one before asking.
        host = qt_oracle.host_window(viewer.frame, control_strip)
        menus = list(walk_menu(viewer.frame.menu))
        if screenshots is not None:
            viewer.view.set_time_range(viewer.pile.tmin, viewer.pile.tmax)
            viewer.view.sortingmode_change()
            viewer.view.set_tracks_range((0, max(1, viewer.view.ntracks)))
            qt_oracle.save_ui_screenshots(viewer.view, host, screenshots)
        return {
            "side": "qt",
            "menus": menus,
            "snufflings": loaded,
            "controls": control_entries,
            "help": help_tables(),
            # Probing keys drives real handlers, so it goes last: by the time it runs
            # everything else has already been read off an undisturbed viewer.
            "keys": key_bindings(viewer.view, viewer.frame.menu),
        }
    finally:
        # `host` stays None when an earlier step raised; closing it then would
        # shadow the real error with an UnboundLocalError.
        if host is not None:
            host.close()
        viewer.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, help="write JSON here instead of stdout")
    parser.add_argument("--screenshots", type=Path)
    args = parser.parse_args()
    inventory = collect(screenshots=args.screenshots)
    text = json.dumps(inventory, indent=2, sort_keys=False) + "\n"
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text)
        print(
            f"qt inventory: {len(inventory['menus'])} menu entries, "
            f"{len(inventory['snufflings'])} snufflings, "
            f"{len(inventory['controls'])} controls, "
            f"{len(inventory['keys'])} bound keys -> {args.out}"
        )
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
