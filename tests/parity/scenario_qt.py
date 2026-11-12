"""Tier 3: drive the offscreen Qt snuffler through a scenario's steps.

The same widget the user clicks, driven the way the user drives it — menu actions
triggered, keys delivered as real key events, picks placed through the real mouse
handlers: double-click, swallowed release, press, release.
After every step it emits one snapshot in the schema ``scenario_web.ts`` also emits, and
:mod:`run_scenarios` diffs the two.

Writes JSON on stdout, or to ``--out``.
"""

from __future__ import annotations

# harness first: it prepares the environment before pyrocko and Qt load.
import harness  # noqa: F401 — imported for its side effects  # isort: skip

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Dict, List

import qt_oracle
from corpus import build as corpus_build
from PyQt5 import QtCore as qc
from PyQt5 import QtGui as qg
from PyQt5 import QtWidgets as qw
from pyrocko.gui.snuffler.marker import EventMarker
from scenario import DATASETS, Scenario, Step, load_all


def find_action(menu: qw.QWidget, path: str) -> qw.QAction:
    """Resolve a '/'-joined menu path to the action it names."""
    parts = path.split("/")
    current: qw.QWidget = menu
    for depth, part in enumerate(parts):
        for action in current.actions():
            if qt_oracle.clean_menu_text(action.text()) == part:
                if depth == len(parts) - 1:
                    return action
                submenu = action.menu()
                if submenu is None:
                    raise KeyError(f"{path}: {part} is not a submenu")
                current = submenu
                break
        else:
            raise KeyError(f"{path}: no menu entry named {part!r}")
    raise KeyError(path)


class QtDriver:
    def __init__(self, waveforms: Path) -> None:
        self.viewer = qt_oracle.QtViewer([waveforms])
        self.frame = self.viewer.frame
        self.view = self.viewer.view
        self.controls = self.frame.controls()
        self.marker_editor = self.frame.marker_editor()
        # Tier 1 needs no window — it only paints — but a driver that presses keys
        # does; see `qt_oracle.host_window`.
        self.window = qt_oracle.host_window(self.frame, self.controls, self.marker_editor)
        self.frame.resize(*qt_oracle.CANVAS_SIZE)
        # Keep fractional picks exact when the dock layout negotiates its minimum size.
        self.view.setFixedSize(*qt_oracle.CANVAS_SIZE)
        self.view.set_time_range(self.viewer.pile.tmin, self.viewer.pile.tmax)
        self.view.sortingmode_change()
        self.view.set_tracks_range((0, max(1, self.view.ntracks)))
        # Sorting in show-all mode adds a pixel margin; fixtures explicitly start at the data bounds.
        self.view.show_all = False
        self.view.set_time_range(self.viewer.pile.tmin, self.viewer.pile.tmax)
        self.settle()

    def settle(self) -> None:
        """Paint once and drain the event loop, so state that only the painter
        computes (the track map, the cutout cache) is current before it is read."""
        self.view.grab()
        qw.QApplication.processEvents()

    def apply(self, step: Step) -> None:
        if step.action == "menu":
            find_action(self.frame.menu, step.target).trigger()
        elif step.action == "key":
            self.press(step.target)
        elif step.action == "filter":
            if step.highpass is not None:
                self.set_control(self.frame.highpass_control, step.highpass)
            if step.lowpass is not None:
                self.set_control(self.frame.lowpass_control, step.lowpass)
        elif step.action == "pick":
            self.pick(step.x, step.y)
        else:
            raise ValueError(step.action)
        self.settle()

    @staticmethod
    def set_control(control: Any, value: float) -> None:
        """Move a ``ValControl`` the way a user's edit does.

        ``set_value`` alone is the *programmatic* setter and deliberately mutes the
        signal, so the viewer would never hear about it.
        Every user-facing path (typing in the box, dragging the slider) ends by calling
        ``fire_valchange``, which is what actually reaches ``lowpass_change`` /
        ``highpass_change``.
        """
        control.set_value(value)
        control.fire_valchange()

    def press(self, token: str) -> None:
        """Deliver one key, or each stroke of a chord, to the viewer.

        Through `QApplication.sendEvent` rather than `QTest.keyClick`, so the event is
        built by the one translator both sides of the harness share
        (`qt_oracle.key_event`) instead of by a second table that could drift from it.
        """
        for stroke in [part.strip() for part in token.split(",") if part.strip()]:
            qw.QApplication.sendEvent(self.view, qt_oracle.key_event(stroke))

    def pick(self, x_fraction: float, y_fraction: float) -> None:
        """Place a marker the way a user's double-click does.

        The real path, driven through the real handlers: ``mouseDoubleClickEvent`` arms
        picking and sets ``ignore_releases`` so the double-click's own release is
        swallowed, then the next press records ``picking_down`` and its release commits
        the pick through ``stop_picking``.
        ``start_picking`` reads the position from the cursor, so the cursor is placed
        first.
        """
        width, height = self.view.width(), self.view.height()
        axis_height = self.view.ax_height
        x = int(x_fraction * width)
        y = int(axis_height + y_fraction * (height - 2 * axis_height))
        position = qc.QPoint(x, y)
        global_position = self.view.mapToGlobal(position)
        qg.QCursor.setPos(global_position)

        def mouse_event(kind: Any) -> Any:
            return qg.QMouseEvent(
                kind,
                qc.QPointF(position),
                qc.QPointF(global_position),
                qc.Qt.LeftButton,
                qc.Qt.LeftButton,
                qc.Qt.NoModifier,
            )

        self.view.mouseDoubleClickEvent(mouse_event(qc.QEvent.MouseButtonDblClick))
        # Swallowed by ignore_releases — the release that ends the double-click.
        self.view.mouseReleaseEvent(mouse_event(qc.QEvent.MouseButtonRelease))
        self.view.mousePressEvent(mouse_event(qc.QEvent.MouseButtonPress))
        self.view.mouseReleaseEvent(mouse_event(qc.QEvent.MouseButtonRelease))

    def snapshot(self, label: str, watch_menus: List[str]) -> Dict[str, Any]:
        view = self.view
        menu_state: Dict[str, bool] = {}
        for name in watch_menus:
            action = find_action(self.frame.menu, name)
            submenu = action.menu()
            if submenu is None:
                continue
            for item in submenu.actions():
                if item.isSeparator() or not item.isCheckable():
                    continue
                # Radio items are excluded: the web port publishes no selected state
                # for its equivalents, so there is nothing to compare them against.
                group = item.actionGroup()
                if group is not None and group.isExclusive():
                    continue
                menu_state[f"{name}/{qt_oracle.clean_menu_text(item.text())}"] = bool(
                    item.isChecked()
                )

        markers = [
            {
                "kind": int(marker.kind),
                "nslc": sorted(".".join(ids) for ids in (marker.nslc_ids or [])),
                "phase": getattr(marker, "_phasename", None) or "",
                "polarity": getattr(marker, "_polarity", None),
                "selected": bool(marker.selected),
                "is_event": isinstance(marker, EventMarker),
                "active": marker is view.get_active_event_marker(),
                "origin_time": marker.tmin if isinstance(marker, EventMarker) else None,
                "time": marker.tmin,
            }
            for marker in view.get_markers()
        ]
        markers.sort(key=lambda item: (item["kind"], item["nslc"], item["phase"]))
        selected = sum(1 for marker in view.get_markers() if marker.selected)

        return {
            "step": label,
            "menu_state": menu_state,
            "controls": {
                "highpass": view.highpass,
                "lowpass": view.lowpass,
                "gain": float(view.gain),
                "rotate": float(view.rotate),
            },
            "markers": markers,
            "selected_count": selected,
            "window": list(view.get_time_range()),
        }

    def close(self) -> None:
        self.viewer.close()


def run(scenario: Scenario, screenshots: Path | None) -> List[Dict[str, Any]]:
    driver = QtDriver(corpus_build.path(DATASETS[scenario.dataset].waveforms))
    try:
        out = [driver.snapshot("start", scenario.watch_menus)]
        for index, step in enumerate(scenario.steps):
            driver.apply(step)
            label = f"{index}:{step.action}:{step.target or step.action}"
            out.append(driver.snapshot(label, scenario.watch_menus))
        if screenshots is not None:
            qt_oracle.save_ui_screenshots(driver.view, driver.window, screenshots / scenario.name)
        return out
    finally:
        driver.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--screenshots", type=Path)
    parser.add_argument("scenario", nargs="*")
    args = parser.parse_args()

    result = {
        scenario.name: run(scenario, args.screenshots)
        for scenario in load_all(tier=3, only=args.scenario or None)
    }
    text = json.dumps(result, indent=2, default=str) + "\n"
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text)
        print(f"qt scenarios: {len(result)} -> {args.out}")
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
