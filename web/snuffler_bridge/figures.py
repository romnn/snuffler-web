"""Figures drawn with pyrocko's own matplotlib code and returned as PNG bytes."""

from __future__ import annotations

import io
import math
import traceback
from typing import Any, Callable, Dict, List, Tuple

import numpy as np
from pyrocko import trace as ptrace
from snuffler.bridge.v1 import figure_pb2, snuffling_pb2

from . import cake, polarization, snuffling_scope, state
from .nslc import NslcId


def _konno_ohmachi_smooth(values: np.ndarray, freqs: np.ndarray, bandwidth: float) -> np.ndarray:
    bandwidth = max(float(bandwidth or 40.0), 1.0)
    values64 = np.asarray(values, dtype=np.float64)
    freqs64 = np.asarray(freqs, dtype=np.float64)
    out = np.empty_like(values64)
    for i, fc in enumerate(freqs64):
        if fc <= 0.0:
            out[i] = values64[i]
            continue
        with np.errstate(divide="ignore", invalid="ignore"):
            x = bandwidth * np.log10(freqs64 / fc)
            w = (np.sin(x) / x) ** 4
        w[~np.isfinite(w)] = 0.0
        w[i] = 1.0
        denom = float(w.sum())
        out[i] = float(np.sum(w * values64) / denom) if denom > 0.0 else values64[i]
    return out


class _FigureEmpty(Exception):
    """Raised by a figure draw function when there is nothing to plot.

    `_render_png` turns it into a FigureResult ``error`` the frontend
    shows as text."""


_matplotlib_ready = False


def _ensure_matplotlib() -> Any:
    """The matplotlib module, switched once to the headless Agg backend.

    Never the Pyodide HTML5 canvas backend: figures are encoded as PNG bytes.
    """
    global _matplotlib_ready
    import matplotlib

    if not _matplotlib_ready:
        matplotlib.use("Agg")
        _matplotlib_ready = True
    return matplotlib


def _render_png(
    request: figure_pb2.FigureRequest, draw: Callable[[Any], None]
) -> figure_pb2.FigureResult:
    """Render a matplotlib figure headlessly and return it as PNG bytes.

    ``draw`` runs the same pyrocko/matplotlib plotting code the Qt snuffler
    does.
    Sizing follows the egui window (device pixels and dpi), and ``dark`` picks
    matplotlib's dark style to match the app theme."""
    _ensure_matplotlib()
    import matplotlib.style as mstyle
    from matplotlib.backends.backend_agg import FigureCanvasAgg
    from matplotlib.figure import Figure

    dpi = float(request.dpi) if request.dpi and request.dpi > 0 else 100.0
    w_px = int(request.width_px) or 640
    h_px = int(request.height_px) or 480
    w_in = max(1.0, w_px / dpi)
    h_in = max(1.0, h_px / dpi)
    style = "dark_background" if request.dark else "default"
    buf = io.BytesIO()
    with mstyle.context(style):
        fig = Figure(figsize=(w_in, h_in), dpi=dpi)
        FigureCanvasAgg(fig)
        try:
            draw(fig)
        except _FigureEmpty as exc:
            return figure_pb2.FigureResult(error=str(exc))
        fig.savefig(buf, format="png", dpi=dpi, facecolor=fig.get_facecolor())
    return figure_pb2.FigureResult(png=buf.getvalue())


def _draw_amplitude_spectra(fig: Any, request: snuffling_pb2.SnufflingRequestParams) -> None:
    """Amplitude spectra of the visible traces, ported from pyrocko's ``AmpSpec.call``.

    As ``snufflings/ampspec.py`` plots them: |spectrum| on log-log axes,
    per-trace ``cm.Accent`` colours, optional Konno-Ohmachi smoothing, and the
    same axis labels, legend and title.
    The FFT (``Trace.spectrum``) is pyrocko's own.
    """
    from matplotlib import cm

    spectrum = request.snufflings.spectrum
    want_smoothing = bool(spectrum.smoothing)
    bandwidth = float(spectrum.smoothing_bandwidth or 40.0)

    # The selected markers' spans *with their channel patterns*, else the
    # visible window — the run scope Qt gives every snuffling
    # (`chopper_selected_traces`, ampspec.py:61).
    # The batches arrive marker-outer, so the per-segment colours follow Qt's
    # order, and the chopper degaps: a channel split across files contributes
    # one spectrum, not one per fragment.
    segs: List[ptrace.Trace] = []
    for batch in snuffling_scope.chopper_selected_traces(request):
        for segment in batch.traces:
            if segment.ydata is None or len(segment.ydata) < 8:
                continue
            segment.ydata = segment.ydata.astype(np.float64)
            segs.append(segment)

    if not segs:
        raise _FigureEmpty("No data in the visible window.")

    axes = fig.add_subplot(111)
    colors = cm.Accent(np.linspace(0.0, 1.0, len(segs)))
    alpha = 0.2 if want_smoothing else 1.0
    flimits: List[float] = []
    alimits: List[float] = []
    for i, segment in enumerate(segs):
        segment.ydata = segment.ydata - segment.ydata.mean()
        freq, spec = segment.spectrum()
        absa = np.abs(spec)
        if freq.size > 2:
            flimits.append(float(freq[1]))
            flimits.append(float(freq[-1]))
        # The y-limit statistics run over the positive bins only: Qt's bare
        # `log(0)` propagates -inf into mean and std, and one flatlined channel
        # would drag the shared axis down and squash every real spectrum.
        positive = absa[absa > 0.0]
        if positive.size:
            labsa = np.log(positive)
            std = float(np.std(labsa))
            mean = float(np.mean(labsa))
            alimits.append(math.exp(mean - 5.0 * std))
            alimits.append(math.exp(mean + 5.0 * std))
        color = colors[i]
        label = ".".join(segment.nslc_id)
        axes.plot(freq, absa, c=color, alpha=alpha, label=label)
        if want_smoothing:
            smoothed = _konno_ohmachi_smooth(absa, freq, bandwidth)
            axes.plot(freq, np.abs(smoothed), c=color, alpha=1.0, label=label)

    axes.set_xscale("log")
    axes.set_yscale("log")
    if alimits:
        axes.set_ylim(min(alimits), max(alimits))
    if flimits:
        axes.set_xlim(min(flimits), max(flimits))
    axes.set_xlabel("Frequency [Hz]")
    axes.set_ylabel("Counts")
    handles, labels = axes.get_legend_handles_labels()
    leg = dict(zip(labels, handles, strict=True))
    if len(segs) > 1:
        axes.legend(
            list(leg.values()),
            list(leg.keys()),
            loc=2,
            borderaxespad=0.0,
            bbox_to_anchor=(1.05, 1.0),
        )
        fig.subplots_adjust(right=0.8, left=0.1)
    else:
        axes.set_title(list(leg.keys())[0], fontsize=16)
        fig.subplots_adjust(right=0.9, left=0.1)


def _spectrogram_colormap(name: str) -> Any:
    """The spectrogram snuffling's colour tables, built exactly as
    ``snufflings/spectrogram.py`` does: a desaturated Tango-colour ramp for
    ``spectro``, else any matplotlib colormap via pyrocko's ``mpl_get_cmap``."""
    from matplotlib.colors import LinearSegmentedColormap
    from pyrocko import plot as pplot

    if name == "spectro":
        cols = [
            pplot.tango_colors[x]
            for x in ["skyblue1", "chameleon1", "butter1", "orange1", "scarletred1", "plum3"]
        ]

        def to01(c: Any) -> Tuple[float, float, float]:
            return (c[0] / 255.0, c[1] / 255.0, c[2] / 255.0)

        def desat(c: Tuple[float, float, float], a: float) -> Tuple[float, ...]:
            mean = (c[0] + c[1] + c[2]) / 3.0
            return tuple(cc * a + mean * (1.0 - a) for cc in c)

        return LinearSegmentedColormap.from_list("spectro", [desat(to01(c), 0.8) for c in cols])
    return pplot.mpl_get_cmap(name)


def spectrogram_inventory(
    request: snuffling_pb2.SnufflingRequestParams, tinc: float, tpad: float
) -> Tuple[float, float, List[Tuple[NslcId, float]]]:
    """Prescan Qt's chopper stream into unique channel/sample-rate panels."""
    times: List[float] = []
    sampling: Dict[NslcId, "set[float]"] = {}
    for batch in snuffling_scope.chopper_selected_traces(
        request,
        mode="inview",
        tinc=tinc,
        tpad=tpad,
        want_incomplete=False,
        load_data=False,
    ):
        times.extend((float(batch.tmin), float(batch.tmax)))
        for trace in batch.traces:
            sampling.setdefault(trace.nslc_id, set()).add(float(trace.deltat))
    if not times or not sampling:
        raise _FigureEmpty("No visible traces to compute a spectrogram for.")
    panels = sorted((nslc, deltat) for nslc, values in sampling.items() for deltat in values)
    return min(times), max(times), panels


def _draw_spectrogram(fig: Any, request: snuffling_pb2.SnufflingRequestParams) -> None:
    """Spectrogram of each visible channel — a faithful port of pyrocko's
    ``snufflings/spectrogram.py``: an absolute window grid (NaN where a window is
    incomplete), pyrocko's PSD normalisation, the `nt_max`/`nf_max` size guards,
    one *shared* colour scale with a colorbar so the panels are comparable, a
    formatted time axis, and the `2/twin` frequency floor."""
    p = request.snufflings.spectrogram
    twin = float(p.window_length_s or 100.0)
    overlap = float(p.overlap_percent or 75.0)
    taper_name = p.taper or "Hanning"
    fmin = float(p.fmin) if p.HasField("fmin") else None
    fmax = float(p.fmax) if p.HasField("fmax") else None
    color_scale = p.color_scale or "log"
    # Qt's image-size guards (spectrogram.py:91).
    nt_max, nf_max = 2000, 500

    def downsample_plan(n: int, n_max: int) -> Tuple[int, int]:
        n_mean = (n - 1) // n_max + 1
        return n // n_mean, n_mean

    tapers = {
        "Hanning": np.hanning,
        "Hamming": np.hamming,
        "Blackman": np.blackman,
        "Bartlett": np.bartlett,
    }
    taper_fn = tapers.get(taper_name, np.hanning)
    cmap = _spectrogram_colormap(p.color_table or "spectro")

    tpad = twin * overlap / 100.0 * 0.5
    tinc = twin - 2.0 * tpad
    if tinc <= 0.0:
        raise _FigureEmpty("Overlap is too high for the window length.")
    tmin, tmax, panels = spectrogram_inventory(request, tinc, tpad)

    nt = int(round((tmax - tmin) / tinc))
    if nt > nt_max:
        _, nt_mean = downsample_plan(nt, nt_max)
        raise _FigureEmpty(
            "Number of samples in spectrogram time axis: %i. Resulting image "
            "will be unreasonably large. Consider increasing window length to "
            "about %g s." % (nt, (tinc * nt_mean) / (1.0 - overlap / 100.0))
        )
    if nt < 1:
        raise _FigureEmpty("The window is shorter than one spectrogram column.")

    n = len(panels)
    ncols = (n - 1) // 5 + 1
    nrows = (n - 1) // ncols + 1
    plotted = 0
    zvalues: List[float] = []
    meshes: List[Any] = []
    axes_list: List[Any] = []
    for i, (nslc, deltat) in enumerate(panels):
        nsamples_want = int(math.floor((tinc + 2.0 * tpad) / deltat))
        if nsamples_want < 4:
            continue
        nfft = 1 << (nsamples_want - 1).bit_length()  # next power of two
        df = 1.0 / (deltat * nfft)
        nf = nfft // 2 + 1
        ifmin = int(math.ceil(fmin / df)) if fmin else 0
        ifmax = min(int(math.floor(fmax / df)) + 1, nf) if fmax else nf
        nf_show = ifmax - ifmin
        if nf_show < 2:
            continue
        nf_show_ds, nf_mean = downsample_plan(nf_show, nf_max)
        win = taper_fn(nsamples_want)
        wnorm = deltat * 2.0 / (df * float(np.sum(win**2)))
        freqs = np.arange(ifmin, ifmax) * df
        if nf_mean != 1:
            nf_trim = (nf_show // nf_mean) * nf_mean
            freqs = np.mean(freqs[:nf_trim].reshape((-1, nf_mean)), axis=1)
        amps = np.full((nf_show_ds, nt), np.nan)
        have_data = False

        def wanted_trace(trace: ptrace.Trace, expected: NslcId = nslc) -> bool:
            return trace.nslc_id == expected

        for batch in snuffling_scope.chopper_selected_traces(
            request,
            mode="inview",
            trace_filter=wanted_trace,
            tinc=tinc,
            tpad=tpad + deltat,
            want_incomplete=False,
        ):
            traces = batch.traces
            if len(traces) != 1:
                continue
            trace = traces[0]
            if float(trace.deltat) != deltat:
                raise ValueError(
                    "Unexpected sampling rate on channel %s: %g"
                    % (".".join(nslc), float(trace.deltat))
                )
            trace.chop(float(batch.tmin) - tpad, trace.tmax, inplace=True)
            if trace.ydata is None:
                continue
            trace.set_ydata(trace.ydata[:nsamples_want])
            if trace.data_len() != nsamples_want:
                continue
            y = trace.ydata.astype(np.float64)
            y = (y - y.mean()) * win
            a = np.abs(np.fft.rfft(y, n=nfft))[ifmin:ifmax] ** 2 * wnorm
            if nf_mean != 1:
                nf_trim = (nf_show // nf_mean) * nf_mean
                a = np.mean(a[:nf_trim].reshape((-1, nf_mean)), axis=1)
            tmid = 0.5 * (float(trace.tmax) + float(trace.tmin))
            index = int(round((tmid - tmin) / tinc))
            if index < 0 or nt <= index:
                continue
            amps[:, index] = a
            have_data = True
        if not have_data:
            continue
        times = tmin + 0.5 * tinc + tinc * np.arange(nt)
        if color_scale == "log":
            with np.errstate(divide="ignore"):
                amps = np.log(amps)
        elif color_scale == "sqrt":
            amps = np.sqrt(amps)
        finite = amps[np.isfinite(amps)]
        if not finite.size:
            continue
        mean_a, std_a = float(np.mean(finite)), float(np.std(finite))
        zvalues.append(max(float(np.min(finite)), mean_a - 3.0 * std_a))
        zvalues.append(min(float(np.max(finite)), mean_a + 3.0 * std_a))
        icol, irow = i % ncols, i // ncols
        axes = fig.add_subplot(nrows, ncols, irow * ncols + icol + 1)
        mesh = axes.pcolormesh(times, freqs, amps, cmap=cmap, shading="gouraud")
        # Qt's frequency floor: `2/twin` when no explicit fmin is set
        # (spectrogram.py:324) — below that a window holds under two cycles.
        axes.set_ylim(fmin if fmin else 2.0 / twin, fmax if fmax else float(freqs[-1]))
        axes.set_title(".".join(code for code in nslc if code), fontsize=9)
        axes.set_ylabel("Frequency [Hz]")
        axes.grid(color="black", alpha=0.3)
        try:
            from pyrocko import plot as pplot

            pplot.mpl_time_axis(axes, 10.0 / ncols)
        except Exception:  # noqa: BLE001 — fall back to epoch seconds
            pass
        meshes.append(mesh)
        axes_list.append(axes)
        plotted += 1

    if plotted == 0:
        raise _FigureEmpty("Not enough data in the window for the chosen window length.")
    # One shared colour scale plus a colorbar (Qt's `psd` colour dim), so the
    # panels are comparable.
    zmin, zmax = min(zvalues), max(zvalues)
    for mesh in meshes:
        mesh.set_clim(zmin, zmax)
    label = {"log": "log PSD", "sqrt": "sqrt PSD"}.get(color_scale, "PSD")
    fig.colorbar(meshes[-1], ax=axes_list, label=label)


# "Fix Scale" (polarization panel): the per-station amplitude scale of the last
# run, reused while the switch is on so successive windows plot comparably.
_polarization_fixed_amax: Dict[Tuple[str, str, str], float] = {}

_POLARIZATION_COLORS = {
    "line": "#204a87",  # sky blue 3 (tango)
    "rot": "#a40000",  # scarlet red 3
    "bazi": "#4e9a06",  # chameleon 3
    "ell": "#75507b",  # plum 2
    "win": (0.9, 0.9, 0.7),
}


def _draw_polarization(fig: Any, request: snuffling_pb2.SnufflingRequestParams) -> None:
    """Per-station polarization figure, ported from ``snufflings/polarization.py``.

    The analysis windows come from the *selected markers* (`get_selected`,
    polarization.py:205; the snuffling fails without one), shifted and sized by
    Offset and Length.
    Each station gets three covariance-hodogram projections with a moving dot,
    optional 2D eigensystem ellipses, a backazimuth indicator, and the rotated
    trace panel labelled with the rotated channel names.
    The eigen-analysis (``polarization.pca``) is pyrocko's math; Qt's
    interactive axes placement becomes a static grid.
    """
    from matplotlib import patches

    pol = request.snufflings.polarization
    t_length = float(pol.window_length_s or 1.0)
    delta_azi = float(pol.azimuth_deg)
    rotate_to_rt = bool(pol.rotate_to_rt)
    dot_position = min(1.0, max(0.0, float(pol.dot_position)))
    show_2d = bool(pol.show_eigensystem_2d)
    show_3d = bool(pol.show_eigensystem_3d)
    fix_scale = bool(pol.fix_scale)
    hp = pol.highpass if pol.HasField("highpass") else None
    lp = pol.lowpass if pol.HasField("lowpass") else None

    windows = polarization.windows(request)
    if not windows:
        raise _FigureEmpty(polarization.NO_MARKERS_TEXT)

    bazis: Dict[Tuple[str, str, str], float] = {}
    if state.active_event is not None and state.stations:
        from pyrocko import orthodrome as od

        elat, elon = state.active_event[0], state.active_event[1]
        for nsl, _chagroup, _wt0, _wt1 in windows:
            station = state.station_at(nsl[0], nsl[1], nsl[2])
            if station is not None:
                bazis[nsl] = float(od.azimuth(station[0], station[1], elat, elon))

    stations = []
    for nsl, chagroup, w_tmin, w_tmax in windows:
        by_channel: Dict[str, ptrace.Trace] = {}
        for cha, segment in polarization.window_channels(
            nsl, chagroup, w_tmin, w_tmax, hp, lp, t_length
        ).items():
            # Demean by the *analysis* chop's mean (polarization.py:326): a
            # DC offset displaces the hodogram and corrupts the covariance.
            try:
                seg_chop = segment.chop(w_tmin, w_tmax, inplace=False)
            except Exception:  # noqa: BLE001 — window not covered
                continue
            if seg_chop.ydata is None or not len(seg_chop.ydata):
                continue
            segment.ydata = segment.ydata - float(np.mean(seg_chop.ydata))
            by_channel[cha] = segment
        group = polarization.order_components(by_channel) if by_channel else None
        if group is None or len(group) != 3:
            continue
        stations.append((nsl, group, w_tmin, w_tmax))

    if not stations:
        raise _FigureEmpty(
            "No matching traces. Check time and channel settings. Traces may "
            "not contain gaps within the extracted time window and in the "
            "padding areas left and right."
        )

    nst = len(stations)
    drawn = 0
    for i, (nsl, group, w_tmin, w_tmax) in enumerate(stations):
        te, tn, tz = group
        ye, yn, yz = (np.asarray(t.ydata, dtype=np.float64) for t in (te, tn, tz))
        ns = min(len(yn), len(ye), len(yz))
        if ns < 3:
            continue
        yn, ye, yz = yn[:ns], ye[:ns], yz[:ns]

        bazimuth = delta_azi
        if rotate_to_rt:
            if nsl not in bazis:
                raise _FigureEmpty(
                    "Cannot rotate to RT. Station coordinates must be available "
                    'and an event must be marked as the "active" event.'
                )
            bazimuth += bazis[nsl] + 90.0
        if bazimuth != 0.0:
            ca, sa = math.cos(math.radians(bazimuth)), math.sin(math.radians(bazimuth))
            # pyrocko's `trace.rotate` matrix: n' = cos·n + sin·e,
            # e' = −sin·n + cos·e.
            yn_r, ye_r = ca * yn + sa * ye, -sa * yn + ca * ye
            cha_e_rot, cha_n_rot = te.channel[-1] + "'", tn.channel[-1] + "'"
        else:
            yn_r, ye_r = yn, ye
            cha_e_rot, cha_n_rot = te.channel[-1], tn.channel[-1]

        amax = float(np.sqrt(yn**2 + ye**2 + yz**2).max()) or 1.0
        # Fix Scale: hold the station's amplitude scale from the previous run
        # so successive windows plot comparably; off, the store follows along.
        if fix_scale and nsl in _polarization_fixed_amax:
            amax = _polarization_fixed_amax[nsl]
        else:
            _polarization_fixed_amax[nsl] = amax
        ipos = int(round(dot_position * (ns - 1)))
        line, rot = _POLARIZATION_COLORS["line"], _POLARIZATION_COLORS["rot"]

        ax01 = fig.add_subplot(nst, 4, i * 4 + 1)
        ax02 = fig.add_subplot(nst, 4, i * 4 + 2)
        ax12 = fig.add_subplot(nst, 4, i * 4 + 3)
        axtr = fig.add_subplot(nst, 4, i * 4 + 4)

        # Panel labels are Qt's: the unrotated horizontals first, then each
        # *rotated channel name* against Z (polarization.py:482) — never a
        # literal R/T, which for a transverse-first rotation reads backwards.
        for axes, xdata, ydata, xlab, ylab in (
            (ax01, ye, yn, te.channel[-1], tn.channel[-1]),
            (ax02, ye_r, yz, cha_e_rot, "Z"),
            (ax12, yn_r, yz, cha_n_rot, "Z"),
        ):
            axes.set_aspect(1.0)
            axes.plot(xdata, ydata, color=line, lw=1.0)
            axes.plot([xdata[ipos]], [ydata[ipos]], "o", ms=4, color=line)
            axes.set_xlim(-amax * 1.05, amax * 1.05)
            axes.set_ylim(-amax * 1.05, amax * 1.05)
            axes.set_xlabel(xlab)
            axes.set_ylabel(ylab)
            axes.set_xticks([])
            axes.set_yticks([])
            if show_2d and xdata.size >= 3:
                _cov, evals, evecs, _azi, _inc = polarization.pca(np.vstack([xdata, ydata]))
                axes.add_patch(
                    patches.Ellipse(
                        (0.0, 0.0),
                        width=math.sqrt(abs(float(evals[0]))) * 2.0,
                        height=math.sqrt(abs(float(evals[1]))) * 2.0,
                        angle=math.degrees(math.atan2(float(evecs[1, 0]), float(evecs[0, 0]))),
                        zorder=-10,
                        fc=_POLARIZATION_COLORS["ell"],
                        ec=_POLARIZATION_COLORS["ell"],
                        alpha=0.3,
                    )
                )

        # Show 3D Eigensystem: the principal axis of the 3-component
        # covariance, projected into each hodogram's coordinate pair.
        if show_3d and ns >= 3:
            _c3, ev3, evec3, _a3, _i3 = polarization.pca(np.vstack([ye_r, yn_r, yz]))
            p_e, p_n, p_z = (float(evec3[j, -1]) for j in range(3))
            scale3 = amax
            for axes, dx, dy in ((ax01, p_e, p_n), (ax02, p_e, p_z), (ax12, p_n, p_z)):
                axes.plot(
                    [-scale3 * dx, scale3 * dx],
                    [-scale3 * dy, scale3 * dy],
                    "-",
                    lw=2,
                    zorder=-15,
                    color=_POLARIZATION_COLORS["ell"],
                    alpha=0.6,
                )

        if nsl in bazis:
            eb = bazis[nsl]
            ax01.plot(
                [0.0, amax * math.cos(math.radians(90.0 - eb))],
                [0.0, amax * math.sin(math.radians(90.0 - eb))],
                "--",
                lw=3,
                zorder=-20,
                color=_POLARIZATION_COLORS["bazi"],
            )

        deltat = float(tz.deltat)
        times = float(tz.tmin) + np.arange(ns) * deltat
        for itr, (ydata, _lab) in enumerate(((ye_r, cha_e_rot), (yn_r, cha_n_rot), (yz, "Z"))):
            col = rot if (itr < 2 and bazimuth != 0.0) else line
            axtr.plot(times, ydata / (2.0 * amax) + itr, color=col, lw=1.0)
            axtr.plot([times[ipos]], [ydata[ipos] / (2.0 * amax) + itr], "o", ms=4, color=col)
        # The shaded band is the *analysis* window inside the padded traces.
        axtr.axvspan(w_tmin, w_tmax, color=_POLARIZATION_COLORS["win"], zorder=-20)
        axtr.set_ylim(-1, 3)
        axtr.set_yticks([0, 1, 2])
        axtr.set_yticklabels([cha_e_rot, cha_n_rot, "Z"])
        axtr.set_xlabel("Time [s]")
        ax01.set_title(".".join(x for x in nsl if x) or nsl[1])
        drawn += 1

    if drawn == 0:
        raise _FigureEmpty("No 3-component data in the window.")
    fig.tight_layout()


def _draw_cake_model(fig: Any, request: snuffling_pb2.SnufflingRequestParams) -> None:
    """Cake 'Plot Model': pyrocko's own ``cake_plot.my_model_plot`` figure."""
    from pyrocko.plot import cake_plot

    model, _phases, _tshift, _use_station_depth, _mode = cake.settings(request)
    mod = cake.load_model(model)
    cake_plot.my_model_plot(mod, fig=fig)


def _draw_cake_rays(fig: Any, request: snuffling_pb2.SnufflingRequestParams) -> None:
    """Cake 'Plot Rays': pyrocko's own ``cake_plot.my_rays_plot`` figure."""
    from pyrocko.plot import cake_plot

    model, phase_names, _tshift, use_station_depth, _mode = cake.settings(request)
    if state.active_event is None or not state.stations:
        raise _FigureEmpty(
            "Cake ray paths need an active event and station coordinates.\n"
            "Search a catalog event and fetch station coordinates first."
        )
    wanted, _errors = cake.phase_definitions(phase_names)
    if not wanted:
        raise _FigureEmpty("No phases selected.")

    mod = cake.load_model(model)
    _elat, _elon, edepth, _etime = state.active_event
    allrays: List[Any] = []
    distances: List[float] = []
    zstop = 0.0
    for _net, _sta, dist_deg, depth in cake.ray_stations():
        rdepth = float(depth) if use_station_depth else 0.0
        allrays.extend(
            mod.arrivals(phases=wanted, distances=[dist_deg], zstart=edepth, zstop=rdepth)
        )
        distances.append(float(dist_deg))
        zstop = rdepth
    if not allrays:
        raise _FigureEmpty("No ray arrivals for the selected phases and distances.")

    # zstart/zstop are depths in metres; distances are epicentral degrees — the
    # units my_rays_plot's axis scaling (labels_rays) expects.
    cake_plot.my_rays_plot(mod, None, allrays, edepth, zstop, distances=distances, fig=fig)


def _draw_beachball(fig: Any, request: figure_pb2.FigureRequest) -> None:
    """Focal-mechanism beachball for a catalog event's moment tensor, drawn by
    pyrocko's matplotlib beachball like the Qt snuffler marker editor."""
    from pyrocko import moment_tensor as mtm
    from pyrocko.plot import tango_colors
    from pyrocko.plot.beachball import plot_beachball_mpl

    m = request.moment_tensor
    moment_tensor = mtm.MomentTensor(
        mnn=m.mnn, mee=m.mee, mdd=m.mdd, mne=m.mne, mnd=m.mnd, med=m.med
    )
    axes = fig.add_subplot(1, 1, 1, aspect=1.0)
    axes.set_axis_off()
    axes.set_xlim(-1.1, 1.1)
    axes.set_ylim(-1.1, 1.1)
    color_t = tuple(c / 255.0 for c in tango_colors["scarletred3"])
    plot_beachball_mpl(
        moment_tensor,
        axes,
        beachball_type="deviatoric",
        size=2.0,
        size_units="data",
        position=(0.0, 0.0),
        color_t=color_t,
        color_p="white",
        edgecolor="white" if request.dark else "black",
        linewidth=1.5,
    )
    strike, dip, rake = moment_tensor.both_strike_dip_rake()[0]
    axes.set_title("strike %.0f°  dip %.0f°  rake %.0f°" % (strike, dip, rake))


_DRAW_BY_KIND: Dict[str, Callable[[Any, snuffling_pb2.SnufflingRequestParams], None]] = {
    "ampspec": _draw_amplitude_spectra,
    "spectrogram": _draw_spectrogram,
    "polarization": _draw_polarization,
    "cake_model": _draw_cake_model,
    "cake_rays": _draw_cake_rays,
}


def render(request: figure_pb2.FigureRequest) -> figure_pb2.FigureResult:
    """Dispatch a FigureRequest to the matching pyrocko/matplotlib figure.

    Any error becomes an ``error`` result the frontend shows as text."""
    if request.kind == "beachball":

        def draw(fig: Any) -> None:
            _draw_beachball(fig, request)

    else:
        draw_kind = _DRAW_BY_KIND.get(request.kind)
        if draw_kind is None:
            return figure_pb2.FigureResult(error="unknown figure: %s" % request.kind)

        def draw(fig: Any) -> None:
            draw_kind(fig, request.params)

    try:
        return _render_png(request, draw)
    except Exception as exc:  # noqa: BLE001 — surface as text, never raise across the worker
        return figure_pb2.FigureResult(
            error="%s: %s\n\n%s" % (type(exc).__name__, exc, traceback.format_exc())
        )
