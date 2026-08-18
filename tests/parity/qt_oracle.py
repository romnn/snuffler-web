"""The Qt oracle: a real, offscreen snuffler, asked what it is about to draw.

Nothing here reimplements snuffler. The harness builds a live ``PileViewerMain``
under ``QT_QPA_PLATFORM=offscreen``, sets the scenario's state through the same
menu actions a user would click, and then *paints the widget*. During that paint it
records the three values the painter computes and immediately turns into pixels:

* the ``prepare_cutout2`` output — the traces about to be drawn,
* the ``trace.minmax`` data ranges — snuffler's scaling matrix, evaluated,
* every ``AutoScaler.make_scale`` call — the per-track y range that follows.

Recording the real paint rather than re-deriving it matters: the moment the harness
recomputes any of this, it stops being an oracle and becomes a second
implementation that can drift the same way the web one can.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import numpy as np
import pyrocko.plot
import pyrocko.trace
from PyQt5 import QtCore as qc
from PyQt5 import QtGui as qg
from PyQt5 import QtWidgets as qw
from pyrocko import model, pile
from pyrocko.io import stationxml
from scenario import CorpusFiles, Scenario

NslcId = Tuple[str, str, str, str]

# Big enough that every corpus dataset fits on screen at once: a scenario should
# never silently compare a page of tracks against the whole set. The eifel dataset
# alone stacks 1061 channel tracks.
NTRACKS_SHOWN_MAX = 2048
CANVAS_SIZE = (1400, 900)

# scenario value -> the label of the radio item in snuffler's own menu definitions.
SCALING_GROUP_ITEMS = {
    "individual": "Individual Scale",
    "common": "Common Scale",
    "per_station": "Common Scale per Station",
    "per_station_location": "Common Scale per Station Location",
    "per_component": "Common Scale per Component",
}
SCALING_BASE_ITEMS = {
    "minmax": "Scaling based on Minimum and Maximum",
    "robust": "Scaling based on Minimum and Maximum (Robust)",
    "mean2": "Scaling based on Mean ± 2x Std. Deviation",
    "mean4": "Scaling based on Mean ± 4x Std. Deviation",
}
SORT_ITEMS = {
    "name": "Sort by Names",
    "distance": "Sort by Distance",
    "azimuth": "Sort by Azimuth",
    "azimuth_blocks": "Sort by Distance in 12 Azimuthal Blocks",
    "backazimuth": "Sort by Backazimuth",
}
SUBSORT_ITEMS = {
    "nslc": "Subsort by Network, Station, Location, Channel",
    "nscl": "Subsort by Network, Station, Channel, Location",
    "sncl": "Subsort by Station, Network, Channel, Location",
    "lnsc": "Subsort by Location, Network, Station, Channel",
    "cnsl": "Subsort by Channel, Network, Station, Location",
    "nsc_l": "Subsort by Network, Station, Channel (Grouped by Location)",
    "snc_l": "Subsort by Station, Network, Channel (Grouped by Location)",
}


@dataclass
class Segment:
    """One trace the renderer was about to draw."""

    nslc: NslcId
    tmin: float
    deltat: float
    y: np.ndarray


@dataclass
class Track:
    index: int
    key: Tuple[Any, ...]
    nslc_ids: List[NslcId]


@dataclass
class QtSnapshot:
    window: Tuple[float, float]
    segments: List[Segment]
    tracks: List[Track]
    data_ranges: Dict[Any, Tuple[float, float]] = field(default_factory=dict)
    # (data range as handed to AutoScaler) -> (ymin, ymax, yinc)
    scales: Dict[Tuple[float, float], Tuple[float, float, float]] = field(default_factory=dict)
    # nslc -> the scaling-group key snuffler filed it under for this scenario.
    scaling_keys: Dict[NslcId, Any] = field(default_factory=dict)
    # (net, sta, loc) and (net, sta) -> event-relative geometry, as `set_origin`
    # computed it. Both of snuffler's station keys, because co-located instruments
    # have their own positions and so their own distances.
    station_geometry: Dict[Tuple[str, ...], Dict[str, Optional[float]]] = field(
        default_factory=dict
    )
    # Trace boxes as `draw_trace_boxes` painted them: per track index, the filled
    # rects in *pixels* — (left, right, palette slot in `box_styles`) — recorded
    # off the real painter. Pixels rather than times, because Qt clips each rect
    # to the canvas (right edge padded by one pixel) before drawing, and
    # un-projecting would launder that back into times the painter never used.
    boxes: Dict[int, List[Tuple[float, float, int]]] = field(default_factory=dict)
    # The time projection the boxes were drawn through, ((tmin, tmax),
    # (umin, umax)): what the comparator needs to put the web's box times into
    # the same pixels. None when Qt never drew boxes this paint.
    box_projection: Optional[Tuple[Tuple[float, float], Tuple[float, float]]] = None
    png: Optional[bytes] = None
    full_png: Optional[bytes] = None


def assert_upstream_geometry() -> None:
    """Fail if upstream stops computing the vertical leg the way the port copies it.

    ``bridge._sort_distance`` mirrors ``Station.set_event_relative_data``, elevation
    bug and all, because parity with the Qt application is what this suite measures —
    see ``vertical-leg-follows-qt`` in ``known_gaps.toml``. The day pyrocko starts
    folding in the elevation, that mirroring turns from parity into a divergence, and
    the only symptom would be every distance sort disagreeing by tens of metres.

    So the formula is asserted directly, on a station carrying *both* an elevation and
    a borehole depth: the three candidate vertical legs give three different answers
    there, and the check names which one upstream still uses. Doing it here rather
    than through a scenario means a ``PYROCKO_REF`` bump trips it even if no corpus
    station happens to have both.
    """
    station = model.Station(network="XX", station="CAL", lat=0.0, lon=0.0, elevation=1700.0)
    station.depth = 145.0
    event = model.Event(lat=0.0, lon=0.0, depth=50000.0)
    station.set_event_relative_data(event, distance_3d=True)
    # Co-located with the event, so the surface leg is zero and `dist_m` *is* the
    # vertical leg — no geodesy in the way of the one term being asserted.
    qt_leg = float(station.dist_m)
    expected = abs(event.depth - station.depth)
    if abs(qt_leg - expected) > 1e-6:
        honest = abs(event.depth + station.elevation - station.depth)
        which = (
            "the honest elevation-aware term" if abs(qt_leg - honest) < 1e-6 else "something else"
        )
        raise SystemExit(
            "upstream pyrocko changed the 3D vertical leg: "
            f"set_event_relative_data gave {qt_leg:.1f} m where "
            f"`event.depth - station.depth` is {expected:.1f} m ({which}).\n"
            "web/bridge.py:_sort_distance deliberately mirrors the old formula — see "
            "`vertical-leg-follows-qt` in tests/parity/known_gaps.toml. Follow upstream "
            "there and retire that entry, or pin pyrocko back."
        )


_APP: Optional[qw.QApplication] = None


def _app() -> qw.QApplication:
    """One QApplication for the whole process; Qt allows no more."""
    global _APP
    if _APP is None:
        existing = qw.QApplication.instance()
        _APP = existing if isinstance(existing, qw.QApplication) else qw.QApplication([])
    return _APP


# The harness's key notation, resolved to Qt. Same tokens the help pages use and the
# same ones `parity_keys.ts` resolves for the browser, so a scenario, an inventory and
# a driver all name a key the same way.
KEY_NAMES: Dict[str, Tuple[int, str]] = {
    "<space>": (qc.Qt.Key_Space, " "),
    "<backspace>": (qc.Qt.Key_Backspace, ""),
    "<delete>": (qc.Qt.Key_Delete, ""),
    "<escape>": (qc.Qt.Key_Escape, ""),
    "<tab>": (qc.Qt.Key_Tab, "\t"),
    "<enter>": (qc.Qt.Key_Return, "\r"),
    "<pageup>": (qc.Qt.Key_PageUp, ""),
    "<pagedown>": (qc.Qt.Key_PageDown, ""),
    "<up>": (qc.Qt.Key_Up, ""),
    "<down>": (qc.Qt.Key_Down, ""),
    "<left>": (qc.Qt.Key_Left, ""),
    "<right>": (qc.Qt.Key_Right, ""),
    ":": (qc.Qt.Key_Colon, ":"),
    "?": (qc.Qt.Key_Question, "?"),
    "+": (qc.Qt.Key_Plus, "+"),
    "-": (qc.Qt.Key_Minus, "-"),
    "=": (qc.Qt.Key_Equal, "="),
    "_": (qc.Qt.Key_Underscore, "_"),
    "/": (qc.Qt.Key_Slash, "/"),
    **{f"<f{index}>": (qc.Qt.Key_F1 + index - 1, "") for index in range(1, 13)},
}


def key_event(token: str) -> Any:
    """Build the ``QKeyEvent`` one harness key token stands for.

    Single characters go through as themselves — snuffler's ``keyPressEvent`` matches
    on ``key_event.text()``, so 'a' and 'A' are different commands and the shift has to
    be real. `<shift> + <tab>` is Qt's Backtab, which is a key of its own.
    """
    modifiers = qc.Qt.NoModifier
    while token.startswith("<shift> + "):
        modifiers |= qc.Qt.ShiftModifier
        token = token[len("<shift> + ") :]

    if token == "<tab>" and modifiers & qc.Qt.ShiftModifier:
        return qg.QKeyEvent(qc.QEvent.KeyPress, qc.Qt.Key_Backtab, modifiers, "")
    if token in KEY_NAMES:
        key, text = KEY_NAMES[token]
        return qg.QKeyEvent(qc.QEvent.KeyPress, key, modifiers, text)
    if len(token) == 1:
        if token.isupper():
            modifiers |= qc.Qt.ShiftModifier
        return qg.QKeyEvent(qc.QEvent.KeyPress, ord(token.upper()), modifiers, token)
    raise KeyError(f"unknown key token {token!r}")


def host_window(frame: Any, controls: Any = None, marker_editor: Any = None) -> Any:
    """Put a `PileViewer` inside the kind of window snuffler expects to live in.

    Not decoration. `keyPressEvent` ends in `update_status()`, which writes to
    `self.window().statusBar()`, and `passband_check()` posts to
    `self.window().status_messages` — both attributes of the real
    `SnufflerWindow`. Without them every key raises, and a harness that catches the
    exception concludes the key was handled.

    Wired the way `snuffler_app.SnufflerWindow` wires it (snuffler_app.py:696), with
    pyrocko's own widget.
    """
    from pyrocko.gui import util as gui_util

    window = qw.QMainWindow()
    window.setCentralWidget(frame)
    window.setMenuBar(frame.menu)
    if controls is not None:
        dock = qw.QDockWidget("Main Controls", window)
        dock.setWidget(controls)
        window.addDockWidget(qc.Qt.BottomDockWidgetArea, dock)
    if marker_editor is not None:
        dock = qw.QDockWidget("Markers", window)
        dock.setWidget(marker_editor)
        window.addDockWidget(qc.Qt.RightDockWidgetArea, dock)
    window.status_messages = gui_util.StatusMessages()
    window.statusBar().addPermanentWidget(window.status_messages)
    window.resize(*CANVAS_SIZE)
    return window


def clean_menu_text(text: str) -> str:
    """Drop Qt's '&' accelerator markers from a menu label, keeping a literal '&&'."""
    return text.replace("&&", "\0").replace("&", "").replace("\0", "&").strip()


def _check_radio(items: List[Tuple[Any, Any]], label: str) -> None:
    """Check the radio action carrying ``label``, exactly as clicking it would."""
    for action, _value in items:
        if action.text() == label:
            action.setChecked(True)
            return
    raise KeyError(f"no menu item {label!r} in {[a.text() for a, _ in items]}")


class QtViewer:
    """A live offscreen snuffler over one corpus dataset."""

    def __init__(self, waveform_paths: List[Path]):
        from pyrocko.gui.snuffler.pile_viewer import PileViewer

        _app()
        self.pile = pile.make_pile([str(path) for path in waveform_paths], show_progress=False)
        # use_opengl defaults to `is_macos`, which would select the QOpenGLWidget
        # subclass and need a GL context the offscreen platform cannot give.
        self.frame = PileViewer(self.pile, ntracks_shown_max=NTRACKS_SHOWN_MAX, use_opengl=False)
        self.frame.resize(*CANVAS_SIZE)
        self.view = self.frame.get_view()

    def load_stations(self, path: Path) -> None:
        self.view.add_stations(model.load_stations(str(path)))

    def load_events(self, path: Path) -> None:
        """Load events and activate the largest — the same choice the web bridge
        makes in ``_load_events_result``, so the sorts share one origin."""
        events = model.load_events(str(path))
        if not events:
            return
        self.view.add_events(events)
        best = max(events, key=lambda ev: ev.magnitude or 0.0)
        self.view.set_active_event(best)

    def load_responses(self, path: Path) -> None:
        """Load one StationXML the way ``snuffler --stationxml`` does.

        Upstream (``snuffler.py:323``) feeds the same document to both consumers:
        ``get_pyrocko_stations()`` gives the sorts their coordinates and the raw
        document joins ``station_xmls`` for restitution. The web bridge harvests
        coordinates from its response files the same way, so loading them must mean
        the same thing on both sides.
        """
        sx = stationxml.load_xml(filename=str(path))
        self.view.add_stations(sx.get_pyrocko_stations())
        self.view.station_xmls.append(sx)

    def apply(self, state: Dict[str, Any]) -> None:
        view = self.view

        # `lowpass_change`/`highpass_change` are the real setters, but they also
        # push a status message through `self.window().status_messages`, which only
        # exists on the full SnufflerWindow. What they do to the viewer is these
        # three lines.
        view.highpass = state["highpass"]
        view.lowpass = state["lowpass"]
        view.tf_cache = {}

        view.rotate = float(state["rotate"])
        view.gain = float(state["gain"])
        view.visible_length = float(state["visible_length"])

        view.menuitem_demean.setChecked(bool(state["demean"]))
        view.menuitem_degap.setChecked(bool(state["degap"]))
        view.menuitem_allowdownsampling.setChecked(bool(state["allow_downsampling"]))
        view.menuitem_fft_filtering.setChecked(bool(state["fft_filtering"]))
        view.menuitem_lphp.setChecked(bool(state["bandpass_lphp"]))
        view.menuitem_remove_resp.setChecked(bool(state["remove_response"]))
        view.menuitem_liberal_fetch.setChecked(bool(state["liberal_fetch"]))
        view.menuitem_fixscalerange.setChecked(bool(state["fix_scale_ranges"]))
        view.menuitem_showscalerange.setChecked(bool(state["show_scale_ranges"]))
        view.menuitem_showscaleaxis.setChecked(bool(state["show_scale_axes"]))
        view.menuitem_distances_3d.setChecked(bool(state["distances_3d"]))

        _check_radio(view.menuitems_scaling, SCALING_GROUP_ITEMS[state["scaling_group"]])
        view.scalingmode_change()
        _check_radio(view.menuitems_scaling_base, SCALING_BASE_ITEMS[state["scaling_base"]])
        view.scaling_base_change()

        _check_radio(view.menuitems_ssorting, SORT_ITEMS[state["sort"]])
        _check_radio(view.menuitems_sorting, SUBSORT_ITEMS[state["subsort"]])
        # `3D distances` only reaches the station table through `set_origin`, which
        # the distance sorts read; re-deriving it here keeps the flag honest.
        event = view.get_active_event()
        if event is not None:
            view.set_origin(event)
        view.s_sortingmode_change()

        view.set_tracks_range((0, max(1, view.ntracks)))

        # Tier 1 is the wiggle pipeline. The waterfall diverges below the seam this
        # oracle watches — Qt paints a QImage from `pile_viewer_waterfall`, the web
        # returns normalized strips — so comparing them would be a pixel diff, which
        # the plan rules out. See known_gaps.toml [[numeric]] "waterfall-pipeline".
        if state["mode"] != "wiggle":
            raise ValueError(
                f"qt_oracle covers the wiggle pipeline only, not mode={state['mode']!r}"
            )

    def snapshot(self, tmin: float, tmax: float, want_png: bool = False) -> QtSnapshot:
        """Paint once, and keep what the painter computed on the way."""
        view = self.view
        view.set_time_range(tmin, tmax)

        cutout: List[Any] = []
        data_ranges: Dict[Any, Tuple[float, float]] = {}
        scales: Dict[Tuple[float, float], Tuple[float, float, float]] = {}
        boxes: Dict[int, List[Tuple[float, float, int]]] = {}
        box_projection: List[Tuple[Tuple[float, float], Tuple[float, float]]] = []

        original_cutout = view.prepare_cutout2
        original_minmax = pyrocko.trace.minmax
        original_make_scale = pyrocko.plot.AutoScaler.make_scale
        original_boxes = view.draw_trace_boxes

        def record_cutout(*args: Any, **kwargs: Any) -> Any:
            traces = original_cutout(*args, **kwargs)
            cutout[:] = traces
            return traces

        def record_minmax(*args: Any, **kwargs: Any) -> Any:
            ranges = original_minmax(*args, **kwargs)
            data_ranges.update(ranges)
            return ranges

        def record_make_scale(self: Any, data_range: Any, *args: Any, **kwargs: Any) -> Any:
            out = original_make_scale(self, data_range, *args, **kwargs)
            scales[(float(data_range[0]), float(data_range[1]))] = (
                float(out[0]),
                float(out[1]),
                float(out[2]),
            )
            return out

        def record_boxes(p: Any, time_projection: Any, track_projections: Any) -> Any:
            """Run the real `draw_trace_boxes` with a painter that keeps each fill.

            The one `fillRect` per box (`drawbox`, pile_viewer.py:2996) carries
            everything the comparison needs: the clipped rect in pixels, and the
            fill brush — a `box_styles` module singleton, so identity recovers the
            palette slot (`istyle % 7`) the pixels actually show.
            """
            from pyrocko.gui.snuffler.pile_viewer import box_styles

            fills: List[Tuple[Any, Any]] = []

            class RecordingPainter:
                def __init__(self, inner: Any) -> None:
                    self._inner = inner

                def fillRect(self, rect: Any, brush: Any) -> Any:
                    fills.append((qc.QRectF(rect), brush))
                    return self._inner.fillRect(rect, brush)

                def __getattr__(self, name: str) -> Any:
                    return getattr(self._inner, name)

            out = original_boxes(RecordingPainter(p), time_projection, track_projections)
            box_projection[:] = [
                (
                    tuple(float(v) for v in time_projection.get_in_range()),
                    tuple(float(v) for v in time_projection.get_out_range()),
                )
            ]
            # Rows are recovered from the rect's top edge: each row projection
            # maps 0 to its own top, so the nearest one names the track.
            tops = [(float(proj(0.0)), itrack) for itrack, proj in track_projections.items()]
            for rect, brush in fills:
                y = float(rect.top())
                itrack = min(tops, key=lambda pair: abs(pair[0] - y))[1]
                color = next(
                    (i for i, style in enumerate(box_styles) if brush is style.fill_brush),
                    -1,
                )
                boxes.setdefault(itrack, []).append(
                    (float(rect.left()), float(rect.right()), color)
                )
            return out

        view.prepare_cutout2 = record_cutout  # type: ignore[method-assign]
        pyrocko.trace.minmax = record_minmax  # type: ignore[assignment]
        pyrocko.plot.AutoScaler.make_scale = record_make_scale  # type: ignore[method-assign]
        view.draw_trace_boxes = record_boxes  # type: ignore[method-assign]
        try:
            pixmap = view.grab()
        finally:
            del view.prepare_cutout2
            pyrocko.trace.minmax = original_minmax  # type: ignore[assignment]
            pyrocko.plot.AutoScaler.make_scale = original_make_scale  # type: ignore[method-assign]
            del view.draw_trace_boxes

        segments = [
            Segment(
                nslc=tuple(tr.nslc_id),
                tmin=float(tr.tmin),
                deltat=float(tr.deltat),
                y=np.asarray(tr.get_ydata(), dtype=np.float64),
            )
            for tr in cutout
        ]

        tracks = [
            Track(
                index=index,
                key=tuple(key) if isinstance(key, tuple) else (key,),
                nslc_ids=sorted(view.track_to_nslc_ids.get(index, set())),
            )
            for index, key in enumerate(view.track_keys)
        ]

        return QtSnapshot(
            window=view.get_time_range(),
            segments=segments,
            tracks=tracks,
            data_ranges={key: (float(lo), float(hi)) for key, (lo, hi) in data_ranges.items()},
            scales=scales,
            scaling_keys={segment.nslc: self.scaling_key(segment.nslc) for segment in segments},
            station_geometry=self.station_geometry(),
            boxes=boxes,
            box_projection=box_projection[0] if box_projection else None,
            png=_pixmap_png(pixmap) if want_png else None,
        )

    def station_geometry(self) -> Dict[Tuple[str, ...], Dict[str, Optional[float]]]:
        """Copy out what ``set_origin`` computed, under both of snuffler's keys.

        The viewer files every station under ``(net, sta, loc)`` *and* ``(net, sta)``
        (``add_stations``, pile_viewer.py:1406) and resolves a trace against them in
        that order, so both belong in the snapshot: the three-element key is the only
        place a borehole sensor's own distance is recorded, and the two-element one is
        the fallback for a trace whose location no station file names.

        Plain floats rather than the ``Station`` objects: the snapshot outlives the
        viewer, and nothing downstream should be reaching back into a closed widget.
        """
        out: Dict[Tuple[str, ...], Dict[str, Optional[float]]] = {}
        for key, station in self.view.stations.items():
            out[tuple(key)] = {
                "dist_m": None if station.dist_m is None else float(station.dist_m),
                "azimuth": None if station.azimuth is None else float(station.azimuth),
                "backazimuth": (
                    None if station.backazimuth is None else float(station.backazimuth)
                ),
            }
        return out

    def scaling_key(self, nslc: NslcId) -> Any:
        """The scaling-group key snuffler would file ``nslc`` under, taken from the
        viewer's own currently-selected key function."""

        class _TraceLike:
            def __init__(self, ids: NslcId) -> None:
                self.nslc_id = ids
                self.network, self.station, self.location, self.channel = ids

        return self.view.scaling_key(_TraceLike(nslc))

    def close(self) -> None:
        self.frame.close()
        self.frame.deleteLater()


def _pixmap_png(pixmap: Any) -> bytes:
    buffer = qc.QBuffer()
    buffer.open(qc.QIODevice.WriteOnly)
    pixmap.save(buffer, "PNG")
    return bytes(buffer.data())


def save_ui_screenshots(view: Any, window: Any, directory: Path) -> None:
    """Save a full hosted window and the waveform canvas it contains."""
    directory.mkdir(parents=True, exist_ok=True)
    window.show()
    qw.QApplication.processEvents()
    view.savesvg(str(directory / "qt-waveforms.png"))
    (directory / "qt-full.png").write_bytes(_pixmap_png(window.grab()))


def run(scenario: Scenario, files: CorpusFiles, want_png: bool = False) -> QtSnapshot:
    """Snapshot one scenario through a freshly built viewer.

    Fresh per scenario on purpose: ``fix_scale_ranges`` and the cutout cache are
    both remembered state, so a reused viewer would let one scenario's history leak
    into the next one's numbers.
    """
    viewer = QtViewer([files.waveforms])
    host = None
    try:
        # In the order the scenario lists them, exactly as a user would load them.
        for extra in scenario.load:
            if extra == "stations" and files.stations is not None:
                viewer.load_stations(files.stations)
            elif extra == "events" and files.events is not None:
                viewer.load_events(files.events)
            elif extra == "responses":
                for response in files.responses:
                    viewer.load_responses(response)
        viewer.apply(scenario.state)
        tmin, tmax = scenario.window(viewer.pile.tmin, viewer.pile.tmax)
        snapshot = viewer.snapshot(tmin, tmax, want_png=want_png)
        if want_png:
            host = host_window(viewer.frame, viewer.frame.controls())
            host.show()
            qw.QApplication.processEvents()
            snapshot.full_png = _pixmap_png(host.grab())
        return snapshot
    finally:
        if host is not None:
            host.close()
        viewer.close()
