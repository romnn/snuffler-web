"""Drive the real, upstream PyQt5 snuffler headless, to compare it with the web app.

Agents use this to check that snuffler-web behaves like upstream snuffler and
draws the same thing: open the same example in both, take the same steps, and
compare the screenshots and `state`.

`serve` starts snuffler through its own command line, so it loads files exactly
as `task snuffler` does, then takes commands on a Unix socket until `quit` or 30
idle minutes.
Every other subcommand sends one command and prints the JSON reply.

Prefer the commands that name what they act on (`menu`, `set`, `press`,
`time-range`, `key`, `type`): they act through snuffler's own widgets and
handlers and are exact.
Pixel `click` and `drag` are the last resort, for what only a position can
express, such as picking a marker on a trace.
Their coordinates are pixels in the screenshot.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import tempfile
import time
import traceback
from pathlib import Path
from typing import Any, Dict, List

SOCKET = Path(os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()) / "snuffler-web-qt.sock"
# The parity oracle's canvas, so screenshots of both sides line up.
WINDOW_SIZE = (1400, 900)
IDLE_TIMEOUT_MS = 30 * 60 * 1000


class CommandError(Exception):
    """A command the driver understood but could not carry out."""


def serve(snuffler_args: List[str]) -> None:
    os.environ["QT_QPA_PLATFORM"] = "offscreen"
    import pyrocko
    from pyrocko.gui.snuffler import snuffler

    # Holds the server for the life of the event loop; Qt would close an
    # unreferenced QLocalServer.
    servers: List[Server] = []

    def launch_hook(window: Any) -> None:
        window.resize(*WINDOW_SIZE)
        window.show()
        servers.append(Server(window))

    # snuffler_from_commandline hands its parsed options to pyrocko.snuffle, which
    # calls launch_hook with the window before its event loop starts.
    snuffle = pyrocko.snuffle
    pyrocko.snuffle = lambda *args, **kwargs: snuffle(*args, launch_hook=launch_hook, **kwargs)
    try:
        snuffler.snuffler_from_commandline(snuffler_args)
    finally:
        SOCKET.unlink(missing_ok=True)


class Server:
    """Answers commands from inside snuffler's own Qt event loop.

    An action that opens a modal dialog does not return until the dialog
    closes, so actions run from a zero timer and are answered from a second
    one: the dialog's nested event loop keeps the socket serviced.
    """

    def __init__(self, window: Any) -> None:
        from PyQt5 import QtCore as qc
        from PyQt5 import QtNetwork as qn

        self.window = window
        self.viewer = window.pile_viewer.viewer
        self.errors: List[str] = []
        # PyQt5 aborts on an exception escaping a slot, snuffler's own included.
        sys.excepthook = lambda *exc: self.errors.append("".join(traceback.format_exception(*exc)))

        qn.QLocalServer.removeServer(str(SOCKET))
        self.server = qn.QLocalServer()
        if not self.server.listen(str(SOCKET)):
            raise SystemExit(f"cannot listen on {SOCKET}: {self.server.errorString()}")
        self.server.newConnection.connect(self.accept)
        self.idle = qc.QTimer(singleShot=True, interval=IDLE_TIMEOUT_MS, timeout=self.quit)
        self.idle.start()
        self.connections: List[Any] = []
        print(f"snuffler driver listening on {SOCKET}", file=sys.stderr, flush=True)

    def accept(self) -> None:
        connection = self.server.nextPendingConnection()
        self.connections.append(connection)
        connection.readyRead.connect(lambda: self.handle(connection))
        connection.disconnected.connect(lambda: self.connections.remove(connection))

    def handle(self, connection: Any) -> None:
        from PyQt5 import QtCore as qc

        def reply(body: Dict[str, Any]) -> None:
            connection.write((json.dumps(body) + "\n").encode())
            connection.flush()

        if not connection.canReadLine():
            return
        self.idle.start()
        request = json.loads(bytes(connection.readLine()).decode())
        command = request.pop("command")
        settle_ms = request.pop("settle_ms", 300)
        if command == "quit":
            reply({"ok": True})
            qc.QTimer.singleShot(0, self.quit)
            return
        if command in ("state", "screenshot"):
            try:
                reply({"ok": True, "result": getattr(self, command)(**request)})
            except Exception as err:  # noqa: BLE001 - every failure goes back to the client
                reply(failure(err))
            return

        outcome: List[Dict[str, Any]] = []
        errors_before = len(self.errors)

        def run() -> None:
            try:
                getattr(self, command.replace("-", "_"))(**request)
            except Exception as err:  # noqa: BLE001 - reported to the client below
                outcome.append(failure(err))

        def answer() -> None:
            body = outcome[0] if outcome else {"ok": True}
            if self.errors[errors_before:]:
                body["snuffler_errors"] = self.errors[errors_before:]
            reply(body)

        qc.QTimer.singleShot(0, run)
        qc.QTimer.singleShot(settle_ms, answer)

    # Reading

    def state(self) -> Dict[str, Any]:
        from PyQt5 import QtWidgets as qw
        from pyrocko import util

        keys = list(self.viewer.track_keys or [])
        low, high = self.viewer.shown_tracks_range or (0, len(keys))
        controls = {}
        for edit in self.window.findChildren(qw.QLineEdit):
            if edit.isVisible() and label_beside(edit):
                controls[label_beside(edit)] = edit.text()
        dialogs = [
            widget.windowTitle()
            for widget in qw.QApplication.topLevelWidgets()
            if widget.isVisible() and widget is not self.window
        ]
        return {
            "tmin": util.time_to_str(self.viewer.tmin),
            "tmax": util.time_to_str(self.viewer.tmax),
            "tracks_shown": [
                ".".join(map(str, key)) for key in keys[max(0, int(low)) : int(high + 0.999)]
            ],
            "tracks_total": len(keys),
            "markers": len(self.viewer.get_markers()),
            "controls": controls,
            "status": self.window.statusBar().currentMessage(),
            "dialogs": dialogs,
        }

    def screenshot(self, path: str) -> str:
        """Save the main window, with any open dialog or menu painted on top."""
        from PyQt5 import QtCore as qc
        from PyQt5 import QtGui as qg
        from PyQt5 import QtWidgets as qw

        canvas = self.window.grab()
        painter = qg.QPainter(canvas)
        for widget in qw.QApplication.topLevelWidgets():
            if widget.isVisible() and widget is not self.window:
                corner = self.window.mapFromGlobal(widget.mapToGlobal(qc.QPoint(0, 0)))
                painter.drawPixmap(corner, widget.grab())
        painter.end()
        target = Path(path).resolve()
        if not canvas.save(str(target)):
            raise CommandError(f"cannot write {target}")
        return str(target)

    # Acting

    def menu(self, path: str) -> None:
        actions = menu_actions(self.window.pile_viewer.menu)
        action = actions.get(normalize(path))
        if action is None:
            raise CommandError(f"no menu item {path!r}; there are: {', '.join(sorted(actions))}")
        action.trigger()

    def key(self, keys: List[str]) -> None:
        from PyQt5 import QtTest as qt

        for token in keys:
            key, modifiers = parse_key(token)
            qt.QTest.keyClick(self.focus(), key, modifiers)

    def type(self, text: str) -> None:
        from PyQt5 import QtTest as qt

        qt.QTest.keyClicks(self.focus(), text)

    def set(self, name: str, value: str) -> None:
        """Set a field, slider, checkbox or choice through the label beside it.

        Every snuffler slider is paired with a field, and typing into the field
        is what a user does and what snuffler validates.
        """
        from PyQt5 import QtCore as qc
        from PyQt5 import QtTest as qt
        from PyQt5 import QtWidgets as qw

        for widget in self.window.findChildren(qw.QWidget):
            if not widget.isVisible():
                continue
            if isinstance(widget, qw.QLineEdit) and normalize(label_beside(widget)) == normalize(
                name
            ):
                self.focus()
                widget.setFocus()
                widget.selectAll()
                qt.QTest.keyClicks(widget, value)
                qt.QTest.keyClick(widget, qc.Qt.Key_Return)
                # Hand the keyboard back to the view, where snuffler's shortcuts
                # live, so a later `key` does not type into this field.
                self.viewer.setFocus()
                return
            if isinstance(widget, qw.QCheckBox) and normalize(widget.text()) == normalize(name):
                if widget.isChecked() != (value.lower() in ("1", "true", "on", "yes")):
                    widget.click()
                return
            if isinstance(widget, qw.QComboBox) and normalize(label_beside(widget)) == normalize(
                name
            ):
                index = widget.findText(value)
                if index < 0:
                    choices = [widget.itemText(i) for i in range(widget.count())]
                    raise CommandError(f"{name!r} has no choice {value!r}; there are: {choices}")
                widget.setCurrentIndex(index)
                # `activated` is the user-choice signal, which a programmatic
                # `setCurrentIndex` does not emit.
                widget.activated[int].emit(index)
                widget.activated[str].emit(value)
                return
        raise CommandError(f"no visible field, checkbox or choice named {name!r}")

    def press(self, text: str) -> None:
        """Click a visible button by its text, such as a dialog's OK."""
        from PyQt5 import QtWidgets as qw

        for window in qw.QApplication.topLevelWidgets():
            for button in window.findChildren(qw.QAbstractButton):
                if button.isVisible() and normalize(button.text()) == normalize(text):
                    button.click()
                    return
        raise CommandError(f"no visible button {text!r}")

    def time_range(self, tmin: str, tmax: str) -> None:
        """Show tmin to tmax, as a zoom or pan to exactly that window would."""
        from pyrocko import util

        self.viewer.set_time_range(util.str_to_time(tmin), util.str_to_time(tmax))
        self.viewer.update()

    def click(self, x: int, y: int) -> None:
        self.drag(x, y, x, y, steps=0)

    def drag(self, x0: int, y0: int, x1: int, y1: int, steps: int = 20) -> None:
        from PyQt5 import QtCore as qc
        from PyQt5 import QtGui as qg
        from PyQt5 import QtTest as qt
        from PyQt5 import QtWidgets as qw

        left = qc.Qt.LeftButton
        origin = self.window.mapToGlobal(qc.QPoint(0, 0))
        target = qw.QApplication.widgetAt(origin + qc.QPoint(x0, y0)) or self.window
        self.focus()

        def send(kind: Any, x: float, y: float, buttons: Any) -> None:
            screen = origin + qc.QPoint(round(x), round(y))
            # Snuffler reads pointer positions back through the cursor.
            qg.QCursor.setPos(screen)
            local = qc.QPointF(target.mapFromGlobal(screen))
            event = qg.QMouseEvent(
                kind, local, local, qc.QPointF(screen), left, buttons, qc.Qt.NoModifier
            )
            qw.QApplication.sendEvent(target, event)

        send(qc.QEvent.MouseButtonPress, x0, y0, left)
        for step in range(1, steps + 1):
            send(
                qc.QEvent.MouseMove,
                x0 + (x1 - x0) * step / steps,
                y0 + (y1 - y0) * step / steps,
                left,
            )
            qt.QTest.qWait(10)
        send(qc.QEvent.MouseButtonRelease, x1, y1, qc.Qt.NoButton)

    def wait(self) -> None:
        """Do nothing; the reply comes after `--settle-ms`."""

    def quit(self) -> None:
        from PyQt5 import QtWidgets as qw

        # Skips the "really quit?" confirmation.
        self.window.instant_close = True
        self.window.close()
        qw.QApplication.quit()

    def focus(self) -> Any:
        """The widget keys go to, after making the window active.

        An offscreen window is never active by itself, and a line edit only
        commits on Return in an active window.
        """
        from PyQt5 import QtWidgets as qw

        window = qw.QApplication.activeModalWidget() or self.window
        qw.QApplication.setActiveWindow(window)
        return qw.QApplication.focusWidget() or self.viewer


def failure(err: Exception) -> Dict[str, Any]:
    return {
        "ok": False,
        "error": str(err) if isinstance(err, CommandError) else traceback.format_exc(),
    }


def normalize(text: str) -> str:
    """Compare labels without Qt's '&' accelerators, case, or a trailing ':' or '...'."""
    return " ".join(text.replace("&", "").lower().rstrip(" :.…").split())


def label_beside(widget: Any) -> str:
    """The label left of `widget` on its row, which is how snuffler names its fields."""
    from PyQt5 import QtWidgets as qw

    row = widget.geometry()
    labels = [
        label
        for label in widget.parentWidget().findChildren(qw.QLabel)
        if label.isVisible()
        and label.parentWidget() is widget.parentWidget()
        and label.geometry().right() <= row.left()
        and abs(label.geometry().center().y() - row.center().y()) < row.height()
    ]
    if not labels:
        return ""
    return normalize(max(labels, key=lambda label: label.geometry().x()).text())


def menu_actions(menu: Any, prefix: str = "") -> Dict[str, Any]:
    found: Dict[str, Any] = {}
    for action in menu.actions():
        if action.isSeparator() or not action.text():
            continue
        path = f"{prefix}{normalize(action.text())}"
        if action.menu() is not None:
            found.update(menu_actions(action.menu(), f"{path}/"))
        else:
            found[path] = action
    return found


def parse_key(token: str) -> Any:
    """`a`, `+`, `?`, `Space`, `Return`, `Shift+Tab` or `Ctrl+a` into a key and modifiers."""
    from PyQt5 import QtCore as qc

    *modifier_names, name = (
        token.rsplit("+", 1) if len(token) > 1 and "+" in token[:-1] else ("", token)
    )
    modifiers = qc.Qt.NoModifier
    for modifier in filter(None, "+".join(modifier_names).split("+")):
        modifiers |= {
            "ctrl": qc.Qt.ControlModifier,
            "shift": qc.Qt.ShiftModifier,
            "alt": qc.Qt.AltModifier,
        }[modifier.lower()]
    if len(name) == 1:
        return name, modifiers
    keys = {
        attr[4:].lower(): getattr(qc.Qt, attr) for attr in dir(qc.Qt) if attr.startswith("Key_")
    }
    if name.lower() not in keys:
        raise CommandError(f"unknown key {name!r}")
    return keys[name.lower()], modifiers


def send(request: Dict[str, Any]) -> Dict[str, Any]:
    # Retry while a driver that was just started is still loading its data.
    deadline = time.monotonic() + 120
    while True:
        try:
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(str(SOCKET))
                client.sendall((json.dumps(request) + "\n").encode())
                return json.loads(client.makefile().readline())
        except (FileNotFoundError, ConnectionRefusedError):
            if time.monotonic() > deadline:
                return {"ok": False, "error": f"no driver on {SOCKET}; start one with `serve`"}
            time.sleep(0.25)


def main() -> int:
    top = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = top.add_subparsers(dest="command", required=True)
    sub.add_parser("serve", help="start snuffler headless; snuffler's own arguments follow --")
    sub.add_parser("state", help="time range, shown tracks, markers, controls, status and dialogs")
    sub.add_parser("screenshot", help="save a PNG of the window").add_argument("path")

    def action(name: str, help_text: str) -> argparse.ArgumentParser:
        parser = sub.add_parser(name, help=help_text)
        parser.add_argument(
            "--settle-ms", type=int, default=300, help="time to react before replying"
        )
        return parser

    action("menu", "trigger a menu item, e.g. 'Sorting/Sort by Distance'").add_argument("path")
    action("press", "click a visible button by its text, e.g. 'OK'").add_argument("text")
    window = action(
        "time-range", "show a time window, e.g. '2018-07-15 13:14:00' '2018-07-15 13:20:00'"
    )
    window.add_argument("tmin")
    window.add_argument("tmax")
    action("key", "press keys: a + ? Space Return Escape Shift+Tab Ctrl+a").add_argument(
        "keys", nargs="+"
    )
    action("type", "type text into the focused widget, e.g. after `key :`").add_argument("text")
    setter = action(
        "set", "set a field, slider, checkbox or choice by its label, e.g. 'Highpass [Hz]' 0.5"
    )
    setter.add_argument("name")
    setter.add_argument("value")
    click = action("click", "left-click a screenshot point, for what has no command above")
    drag = action("drag", "left-drag between two points, e.g. to pick; in the view, down zooms in")
    for parser, names in ((click, ("x", "y")), (drag, ("x0", "y0", "x1", "y1"))):
        for name in names:
            parser.add_argument(name, type=int)
    action("wait", "reply after --settle-ms, e.g. while a snuffling runs")
    action("quit", "close snuffler and stop the driver")

    args, rest = top.parse_known_args()
    if args.command == "serve":
        serve(rest[1:] if rest[:1] == ["--"] else rest)
        return 0
    if rest:
        top.error(f"unrecognized arguments: {' '.join(rest)}")
    reply = send(vars(args))
    print(json.dumps(reply, indent=2, ensure_ascii=False))
    return 0 if reply["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
