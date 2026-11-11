"""The Seismosizer snuffling: synthetic seismograms from a Green's-function store."""

from __future__ import annotations

import math
import traceback
from typing import Callable, Dict, Tuple

import numpy as np
from snuffler.bridge.v1 import snuffling_pb2

from . import events, gf_stores, marker_budget, pyrocko_modules, snuffling_scope, state


def run(request: snuffling_pb2.SnufflingRequestParams) -> snuffling_pb2.SnufflingResult:
    """Synthetic seismograms through ``pyrocko.gf.LocalEngine`` and a mounted
    Green's-functions store, a port of ``snufflings/seismosizer.py``.

    Adds the synthetic traces to the pile and a source event marker."""
    # Qt's `call()` opens with `self.cleanup()` (seismosizer.py:97) — before
    # any validation — so even a failing run clears the previous run's
    # synthetics and marker.
    # Every failure carries the updated scene and the origin tag; the tag is
    # what retires the old event marker.
    state.replace_derived_traces("seismosizer", [])
    state.replace_derived_events("seismosizer", {})

    def refuse(text: str) -> snuffling_pb2.SnufflingResult:
        result = snuffling_pb2.SnufflingResult(
            show_window=True, text=text, scene=state.scene_meta(), origin="seismosizer"
        )
        result.event_state.CopyFrom(events.event_state())
        return result

    try:
        return _synthesize(request, refuse)
    except Exception as exc:  # noqa: BLE001 — surface, don't raise
        # Qt surfaces any exception out of `call()` in the snuffling error
        # dialog with the cleanup above already done.
        # The scene and origin make the frontend drop the previous run's output.
        return refuse("%s: %s\n\n%s" % (type(exc).__name__, exc, traceback.format_exc()))


def _synthesize(
    req: snuffling_pb2.SnufflingRequestParams,
    refuse: Callable[[str], snuffling_pb2.SnufflingResult],
) -> snuffling_pb2.SnufflingResult:
    """The Seismosizer run proper, after `run`'s cleanup: build
    the source and targets, process them through the mounted store, and hand
    every invalid input to ``refuse``."""
    p = req.snufflings.seismosizer
    store_label = p.store or ""
    if not store_label.startswith(gf_stores.LABEL_PREFIX):
        return refuse("Select a mounted GF store first (use “Add GF Store…”).")
    sid = store_label[len(gf_stores.LABEL_PREFIX) :]
    store_dir = state.gf_stores.get(sid)
    if store_dir is None:
        return refuse("GF store %r is not mounted." % sid)

    gf = pyrocko_modules.import_full_gf()
    from pyrocko import model

    km = 1000.0
    if state.active_event is not None:
        elat, elon, _edep, etime = state.active_event
    else:
        elat, elon, etime = 0.0, 0.0, 0.0

    # Qt's four STF choices with the duration applied to the instance —
    # including duration 0, which passes a zero-duration STF object rather than
    # none (`get_stf`, seismosizer.py:65).
    stf_type = p.stf_type or "half sin"
    stf_of = {
        "half sin": gf.HalfSinusoidSTF,
        "triangular": gf.TriangularSTF,
        "boxcar": gf.BoxcarSTF,
    }.get(stf_type)
    if stf_of is None and stf_type != "None":
        # Qt's Choice widget makes an unknown label unrepresentable.
        return refuse("Unknown STF type %r." % stf_type)
    stf = stf_of(duration=float(p.stf_duration_s)) if stf_of is not None else None
    common = dict(
        time=etime + float(p.time_s),
        lat=elat,
        lon=elon,
        north_shift=float(p.north_km) * km,
        east_shift=float(p.east_km) * km,
        depth=float(p.depth_km) * km,
    )
    # Qt's five Seismosizer source snufflings, one per `pyrocko.gf` source type
    # (seismosizer.py:280-560), each built with its own parameter set.
    src_type = p.source_type or "DCSource"
    if src_type == "MTSource":
        from pyrocko import moment_tensor as mtm

        # Relative components normalized to unit moment, then scaled to the
        # magnitude's moment (`get_m6`, seismosizer.py:385).
        rm6 = np.array(
            [
                p.relative_mnn,
                p.relative_mee,
                p.relative_mdd,
                p.relative_mne,
                p.relative_mnd,
                p.relative_med,
            ],
            dtype=float,
        )
        m9 = np.array(mtm.symmat6(*rm6))
        m0_unscaled = math.sqrt(float(np.sum(m9**2))) / math.sqrt(2.0)
        if m0_unscaled == 0.0:
            return refuse("MTSource needs a non-zero relative moment tensor.")
        m9 = m9 / m0_unscaled * mtm.magnitude_to_moment(float(p.magnitude))
        source = gf.MTSource(m6=mtm.to6(m9), stf=stf, **common)
    elif src_type == "SFSource":
        source = gf.SFSource(
            fn=float(p.force_north_n),
            fe=float(p.force_east_n),
            fd=float(p.force_down_n),
            stf=stf,
            **common,
        )
    elif src_type == "RectangularSource":
        source = gf.RectangularSource(
            magnitude=float(p.magnitude),
            strike=float(p.strike_deg),
            dip=float(p.dip_deg),
            rake=float(p.rake_deg),
            length=float(p.length_km) * km,
            width=float(p.width_km) * km,
            nucleation_x=float(p.nucleation_x),
            nucleation_y=float(p.nucleation_y),
            velocity=float(p.rupture_velocity_m_s),
            stf=stf,
            **common,
        )
    elif src_type == "PseudoDynamicRupture":
        source = gf.PseudoDynamicRupture(
            nx=max(1, int(p.patches_along_strike)),
            ny=max(1, int(p.patches_down_dip)),
            slip=float(p.slip_m),
            strike=float(p.strike_deg),
            dip=float(p.dip_deg),
            rake=float(p.rake_deg),
            length=float(p.length_km) * km,
            width=float(p.width_km) * km,
            nucleation_x=float(p.nucleation_x),
            nucleation_y=float(p.nucleation_y),
            gamma=float(p.gamma),
            # Qt hardcodes these (seismosizer.py:577) — except nthreads, which
            # is single here because Pyodide has no thread pool for this.
            nthreads=1,
            pure_shear=True,
            smooth_rupture=True,
            **common,
        )
    elif src_type == "DCSource":
        source = gf.DCSource(
            magnitude=float(p.magnitude),
            strike=float(p.strike_deg),
            dip=float(p.dip_deg),
            rake=float(p.rake_deg),
            stf=stf,
            **common,
        )
    else:
        # The five types above are the closed set Qt's Choice offers.
        return refuse("Unknown source type %r." % src_type)
    source.regularize()

    stations = []
    if state.stations:
        for (net, sta), (slat, slon, sele, sdep) in state.stations_by_name().items():
            stations.append(
                model.Station(
                    network=net, station=sta, lat=slat, lon=slon, elevation=sele, depth=sdep
                )
            )
    else:  # snuffler's fallback: two symmetric synthetic stations
        for lat, lon in [(5.0, 0.0), (-5.0, 0.0)]:
            stations.append(
                model.Station(network="SY", station="%g_%g" % (lat, lon), lat=lat, lon=lon)
            )

    # Qt targets the (loc, cha) pairs present in the *marker-scoped* traces per
    # station (`chopper_selected_traces(fallback=True, mode='visible')`,
    # seismosizer.py:114), always adding ('', N/E/Z).
    # The synthetic codes are `loc + '-syn'`, with azimuth and dip assigned for
    # T/R/1/2 channel endings (seismosizer.py:120).
    # Per marker, a trace must match that marker's channel patterns *and* hold
    # data in its span, so a channel silent across the whole scope yields no
    # synthetic target.
    spans = snuffling_scope.spans(req)
    channels_by_station: Dict[Tuple[str, str], "set[Tuple[str, str]]"] = {}
    for trace in state.visible_traces():
        if not any(
            (selector is None or selector(trace)) and trace.overlaps(span_tmin, span_tmax)
            for span_tmin, span_tmax, selector in spans
        ):
            continue
        net, sta, loc, cha = trace.nslc_id
        channels_by_station.setdefault((net, sta), set()).add((loc, cha))
    for station in stations:
        channels_by_station.setdefault((station.network, station.station), set()).update(
            ("", cha) for cha in "NEZ"
        )

    targets = []
    for station in stations:
        for loc, cha in sorted(channels_by_station.get((station.network, station.station), set())):
            target = gf.Target(
                codes=(station.network, station.station, loc + "-syn", cha),
                quantity="displacement",
                lat=station.lat,
                lon=station.lon,
                depth=station.depth,
                store_id=sid,
                interpolation="nearest_neighbor",
                optimization="enable",
            )
            _azi_fwd, bazi = source.azibazi_to(target)
            if cha.endswith("T"):
                target.dip, target.azimuth = 0.0, bazi + 270.0
            elif cha.endswith("R"):
                target.dip, target.azimuth = 0.0, bazi + 180.0
            elif cha.endswith("1"):
                target.dip, target.azimuth = 0.0, 0.0
            elif cha.endswith("2"):
                target.dip, target.azimuth = 0.0, 90.0
            targets.append(target)

    engine = gf.LocalEngine(store_dirs=[store_dir])
    # Pyodide's mounted (NativeFS/OPFS) filesystem does not support mmap, so open
    # the store without memmap (also fine, just slightly slower, on native builds).
    engine._open_stores[sid] = gf.store.Store(store_dir, use_memmap=False)  # noqa: SLF001
    request = gf.Request(sources=[source], targets=targets)
    request.regularize()
    try:
        traces = engine.process(request, nthreads=0).pyrocko_traces()
    except (gf.meta.OutOfBounds, gf.store_ext.StoreExtError) as exc:
        # Qt's `self.fail(e)` on the same two exceptions (seismosizer.py:204):
        # the message, with the cleanup above already done.
        return refuse(str(exc))

    waveform_type = p.waveform_type or "Displacement [m]"
    if waveform_type.startswith("Velocity"):
        for trace in traces:
            trace.set_ydata(np.diff(trace.ydata) / trace.deltat)
    elif waveform_type.startswith("Acceleration"):
        for trace in traces:
            trace.set_ydata(np.diff(np.diff(trace.ydata)) / trace.deltat**2)
    if waveform_type.endswith(("[nm]", "[nm/s]", "[nm/s^2]")):
        for trace in traces:
            trace.set_ydata(trace.ydata * 1e9)

    result = snuffling_pb2.SnufflingResult(show_window=True, origin="seismosizer")
    markers = marker_budget.MarkerAccumulator(req.marker_budget)
    # Qt adds `EventMarker(source.pyrocko_event())` (seismosizer.py:143): the
    # synthetic event carries its full origin and mechanism, not just a label.
    event = source.pyrocko_event()
    ev_hash = str(event.get_hash())
    # The event's own magnitude, not the dial's: a PseudoDynamicRupture
    # derives its moment from slip and geometry, and an SFSource has none.
    result.text = "%d synthetic trace(s) from a %s%s." % (
        len(traces),
        src_type,
        " (M%.1f)" % float(event.magnitude) if event.magnitude is not None else "",
    )
    marker = snuffling_pb2.SnufMarker()
    marker.tmin = float(source.time)
    marker.is_event = True
    marker.event_name = str(event.name) if event.name else "synthetic %s" % src_type
    if event.magnitude is not None:
        marker.magnitude = float(event.magnitude)
    marker.lat = float(event.lat)
    marker.lon = float(event.lon)
    marker.depth = float(event.depth or 0.0)
    if event.moment_tensor is not None:
        marker.moment_tensor.CopyFrom(events.moment_tensor_message(event.moment_tensor))
    marker.event_hash = ev_hash
    markers.add(marker, strength=float(event.magnitude or 0.0))
    # Commit only now, with every fallible step behind us, so a failure never
    # leaves new synthetics without their marker.
    state.replace_derived_events("seismosizer", {ev_hash: event})
    state.replace_derived_traces("seismosizer", traces)
    result.scene.CopyFrom(state.scene_meta())
    result.event_state.CopyFrom(events.event_state())
    return markers.finish(result)
