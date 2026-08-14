"""Snapshot diffing with per-field tolerances.

Each comparison yields :class:`Finding` objects tagged with a *check kind*. A kind
is what ``known_gaps.toml`` keys its numeric entries on, so a divergence is either
explained by a gap the scenario declared or it fails the run — there is no third
state, and in particular no "loosen the tolerance until it passes".

Tolerances are scale-aware rather than element-wise. The web payload crosses the
bridge as float32, so its intrinsic error is a fraction of the *trace peak*, not of
each individual sample; an element-wise ``rtol`` would demand six-digit agreement on
samples that happen to sit near a zero crossing and fail for no reason.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Dict, Iterable, List, Optional, Sequence, Tuple

import numpy as np
from scenario import Compare, Scenario

# Type-only: the snapshots are duck-typed here, and importing the oracle modules
# for real would pull PyQt5 and the bridge into every consumer of this module.
if TYPE_CHECKING:
    import qt_oracle
    import web_oracle

NslcId = Tuple[str, str, str, str]

# Every check kind the harness can report. known_gaps.toml may only reference these.
# The first group is tier 1 (numbers), the second is tier 3 (UI state).
CHECK_KINDS = (
    "presence",
    "geometry",
    "samples",
    "tracks",
    "scales",
    "annotations",
    "stability",
    "menu_state",
    "controls",
    "markers",
)


_ABSENT = object()


@dataclass
class Finding:
    kind: str
    subject: str
    ok: bool
    detail: str = ""
    metrics: Dict[str, Any] = field(default_factory=dict)

    def label(self) -> str:
        return f"{self.kind}:{self.subject}" if self.subject else self.kind


def _fmt(value: float) -> str:
    if value == 0.0:
        return "0"
    if not math.isfinite(value):
        return str(value)
    return f"{value:.3e}"


def _nslc(ids: NslcId) -> str:
    return ".".join(ids)


def _row(members: Sequence[NslcId]) -> str:
    """One track's membership, with a ``+`` between channels sharing the row."""
    return "+".join(_nslc(nslc) for nslc in members) or "(empty)"


def _by_nslc(segments: Sequence[Any]) -> Dict[NslcId, List[Any]]:
    out: Dict[NslcId, List[Any]] = {}
    for segment in segments:
        out.setdefault(segment.nslc, []).append(segment)
    for group in out.values():
        group.sort(key=lambda seg: seg.tmin)
    return out


def compare_presence(qt: qt_oracle.QtSnapshot, web: web_oracle.WebSnapshot) -> List[Finding]:
    qt_ids = set(segment.nslc for segment in qt.segments)
    web_ids = set(segment.nslc for segment in web.segments)
    missing = sorted(qt_ids - web_ids)
    extra = sorted(web_ids - qt_ids)
    ok = not missing and not extra
    detail = ""
    if not ok:
        parts = []
        if missing:
            parts.append("only in Qt: " + ", ".join(_nslc(i) for i in missing))
        if extra:
            parts.append("only in web: " + ", ".join(_nslc(i) for i in extra))
        detail = "; ".join(parts)
    return [
        Finding(
            kind="presence",
            subject="",
            ok=ok,
            detail=detail,
            metrics={"qt": len(qt_ids), "web": len(web_ids)},
        )
    ]


def _pair_segments(
    qt_group: List[Any], web_group: List[Any]
) -> List[Tuple[Optional[Any], Optional[Any]]]:
    """Pair the segments of one channel by which of them overlap in time.

    Gaps make a single channel arrive as several traces, and the two sides may cut
    them differently, so pairing by index would compare unrelated stretches of
    signal. Greatest overlap is the only pairing that means anything.
    """
    pairs: List[Tuple[Optional[Any], Optional[Any]]] = []
    remaining = list(web_group)
    for qt_segment in qt_group:
        qt_span = (qt_segment.tmin, qt_segment.tmin + qt_segment.deltat * qt_segment.y.size)
        best: Optional[Any] = None
        best_overlap = 0.0
        for web_segment in remaining:
            if not web_segment.uniform:
                # Decimated payloads carry their own time axis; span them from the
                # first and last interleaved point.
                if web_segment.y.size < 4:
                    continue
                span = (
                    web_segment.tmin + float(web_segment.y[0]),
                    web_segment.tmin + float(web_segment.y[-2]),
                )
            else:
                span = (
                    web_segment.tmin,
                    web_segment.tmin + web_segment.deltat * web_segment.y.size,
                )
            overlap = min(qt_span[1], span[1]) - max(qt_span[0], span[0])
            if overlap > best_overlap:
                best, best_overlap = web_segment, overlap
        if best is not None:
            remaining.remove(best)
        pairs.append((qt_segment, best))
    pairs.extend((None, leftover) for leftover in remaining)
    return pairs


def compare_samples(
    qt: qt_oracle.QtSnapshot,
    web: web_oracle.WebSnapshot,
    policy: Compare,
) -> List[Finding]:
    findings: List[Finding] = []
    qt_groups = _by_nslc(qt.segments)
    web_groups = _by_nslc(web.segments)

    for nslc in sorted(set(qt_groups) | set(web_groups)):
        for qt_segment, web_segment in _pair_segments(
            qt_groups.get(nslc, []), web_groups.get(nslc, [])
        ):
            subject = _nslc(nslc)
            if qt_segment is None or web_segment is None:
                side = "web" if qt_segment is None else "Qt"
                findings.append(
                    Finding(
                        kind="geometry",
                        subject=subject,
                        ok=False,
                        detail=f"segment present only on the {side} side",
                    )
                )
                continue
            findings.extend(_compare_pair(subject, qt_segment, web_segment, policy))
    return findings


def _compare_pair(
    subject: str, qt_segment: Any, web_segment: Any, policy: Compare
) -> List[Finding]:
    if not web_segment.uniform:
        return [
            Finding(
                kind="samples",
                subject=subject,
                ok=False,
                detail=(
                    "the web side returned a decimated min/max envelope, Qt returned "
                    f"raw samples at deltat={qt_segment.deltat!r}; there is no "
                    "sample-for-sample comparison to make"
                ),
                metrics={
                    "web_points": int(web_segment.y.size // 2),
                    "qt_deltat": qt_segment.deltat,
                    "qt_nsamples": int(qt_segment.y.size),
                },
            )
        ]

    findings: List[Finding] = []
    dt_qt, dt_web = qt_segment.deltat, web_segment.deltat
    dt_rel = abs(dt_qt - dt_web) / max(abs(dt_qt), 1e-30)
    if dt_rel > 1e-9:
        return [
            Finding(
                kind="geometry",
                subject=subject,
                ok=False,
                detail=f"sample rate differs: Qt deltat={dt_qt!r}, web deltat={dt_web!r}",
                metrics={"deltat_qt": dt_qt, "deltat_web": dt_web},
            )
        ]

    offset = (web_segment.tmin - qt_segment.tmin) / dt_qt
    if abs(offset - round(offset)) > 0.01:
        return [
            Finding(
                kind="geometry",
                subject=subject,
                ok=False,
                detail=(
                    f"the two segments sit on different sample grids (offset {offset:.4f} samples)"
                ),
                metrics={"grid_offset_samples": offset},
            )
        ]

    findings.append(
        Finding(
            kind="geometry",
            subject=subject,
            ok=True,
            metrics={
                "deltat": dt_qt,
                "nsamples_qt": int(qt_segment.y.size),
                "nsamples_web": int(web_segment.y.size),
                "grid_offset_samples": int(round(offset)),
            },
        )
    )

    trim = int(math.ceil(policy.edge_trim_s / dt_qt)) if policy.edge_trim_s else 0
    shift = int(round(offset))
    qt_start = max(0, shift)
    web_start = max(0, -shift)
    n = min(qt_segment.y.size - qt_start, web_segment.y.size - web_start)
    n -= 2 * trim
    if n <= 0:
        return findings + [
            Finding(
                kind="samples",
                subject=subject,
                ok=False,
                detail="no overlapping samples left after edge trimming",
                metrics={"edge_trim_s": policy.edge_trim_s},
            )
        ]

    a = qt_segment.y[qt_start + trim : qt_start + trim + n]
    b = web_segment.y[web_start + trim : web_start + trim + n]
    delta = a - b
    diff = np.abs(delta)
    peak = float(np.max(np.abs(a))) if a.size else 0.0
    rms_qt = float(np.sqrt(np.mean(a * a))) if a.size else 0.0
    rms_diff = float(np.sqrt(np.mean(diff * diff))) if diff.size else 0.0
    max_abs = float(np.max(diff)) if diff.size else 0.0
    # The residual with its own mean removed.
    # A gap entry with `residual = "constant"` (the demean-extent shape) uses this to
    # assert the excused divergence really is a per-segment constant offset and
    # nothing more — see `gaps.evaluate`.
    max_abs_centered = float(np.max(np.abs(delta - np.mean(delta)))) if delta.size else 0.0
    bound = policy.atol + policy.rtol * peak
    ok = bool(max_abs <= bound) and bool(np.all(np.isfinite(b)))

    findings.append(
        Finding(
            kind="samples",
            subject=subject,
            ok=ok,
            detail=("" if ok else f"max|Δ|={_fmt(max_abs)} exceeds atol+rtol*peak={_fmt(bound)}"),
            metrics={
                "n": int(n),
                "max_abs": max_abs,
                "max_abs_centered": max_abs_centered,
                "peak_qt": peak,
                "rel_rms": (rms_diff / rms_qt) if rms_qt else (0.0 if not rms_diff else math.inf),
                "bound": bound,
            },
        )
    )
    return findings


def compare_tracks(qt: qt_oracle.QtSnapshot, web: web_oracle.WebSnapshot) -> List[Finding]:
    """Compare the stacking order, not just the membership.

    Only Qt tracks that actually received a trace are considered: ``track_keys``
    spans the whole pile, while the web only emits tracks that intersect the window,
    and a track with nothing in it is not a layout difference.
    """
    qt_populated = [track for track in qt.tracks if track.nslc_ids]
    # Membership per row, not one flat list of channels: a row can legitimately
    # carry several — the segments of a gapped channel, or every location of a
    # channel under the "Grouped by Location" subsorts — and flattening would call
    # a stack of two rows carrying two channels each the same as four rows of one.
    qt_sequence = [sorted(track.nslc_ids) for track in qt_populated]
    web_sequence = list(web.track_members)

    findings = [
        Finding(
            kind="tracks",
            subject="count",
            ok=len(qt_populated) == len(web_sequence),
            detail=(
                ""
                if len(qt_populated) == len(web_sequence)
                else f"Qt stacks {len(qt_populated)} tracks, the web stacks {len(web_sequence)}"
            ),
            metrics={"qt": len(qt_populated), "web": len(web_sequence)},
        )
    ]

    if qt_sequence == web_sequence:
        findings.append(Finding(kind="tracks", subject="order", ok=True))
    else:
        first = next(
            (
                index
                for index in range(max(len(qt_sequence), len(web_sequence)))
                if qt_sequence[index : index + 1] != web_sequence[index : index + 1]
            ),
            0,
        )
        findings.append(
            Finding(
                kind="tracks",
                subject="order",
                ok=False,
                detail=(
                    f"first difference at position {first}: "
                    f"Qt {_row(qt_sequence[first]) if first < len(qt_sequence) else '-'} "
                    f"vs web {_row(web_sequence[first]) if first < len(web_sequence) else '-'}"
                ),
                metrics={
                    "qt_head": [_row(i) for i in qt_sequence[:6]],
                    "web_head": [_row(i) for i in web_sequence[:6]],
                },
            )
        )
    return findings


def compare_scales(
    qt: qt_oracle.QtSnapshot,
    web: web_oracle.WebSnapshot,
    scenario: Scenario,
) -> List[Finding]:
    """Compare the y range each side is about to draw against, per span.

    Per span rather than per track because snuffler projects per (track, scaling
    key): two sensors sharing a location-grouped row are each drawn against their
    own scale, so a per-row comparison could only check one of them.

    The Qt value is not recomputed here: it is the ``(ymin, ymax, yinc)`` its own
    ``AutoScaler`` produced during the paint, looked up by the data range the
    painter handed it — ``(dymin/gain, dymax/gain)``, gain and all.
    """
    gain = float(scenario.state["gain"])
    policy = scenario.compare
    findings: List[Finding] = []

    for nslc, (web_ymin, web_ymax, _web_yscale) in web.span_scales:
        subject = _nslc(nslc)
        # `None` is the scaling key of the "Common Scale" group, so absence needs a
        # sentinel of its own.
        key = qt.scaling_keys.get(nslc, _ABSENT)
        if key is _ABSENT or key not in qt.data_ranges:
            findings.append(
                Finding(
                    kind="scales",
                    subject=subject,
                    ok=False,
                    detail="Qt computed no data range for this track",
                )
            )
            continue
        dymin, dymax = qt.data_ranges[key]
        lookup = (dymin / gain, dymax / gain)
        if lookup not in qt.scales:
            findings.append(
                Finding(
                    kind="scales",
                    subject=subject,
                    ok=False,
                    detail="Qt never scaled this data range during the paint",
                    metrics={"data_range": list(lookup)},
                )
            )
            continue
        qt_ymin, qt_ymax, _yinc = qt.scales[lookup]
        span = max(abs(qt_ymax - qt_ymin), 1e-300)
        dmin = abs(qt_ymin - web_ymin)
        dmax = abs(qt_ymax - web_ymax)
        bound = policy.atol + policy.scale_rtol * span
        ok = dmin <= bound and dmax <= bound
        findings.append(
            Finding(
                kind="scales",
                subject=subject,
                ok=ok,
                detail=(
                    ""
                    if ok
                    else (
                        f"y range Qt [{_fmt(qt_ymin)}, {_fmt(qt_ymax)}] vs "
                        f"web [{_fmt(web_ymin)}, {_fmt(web_ymax)}]"
                    )
                ),
                metrics={
                    "qt": [qt_ymin, qt_ymax],
                    "web": [web_ymin, web_ymax],
                    "rel_ymin": dmin / span,
                    "rel_ymax": dmax / span,
                },
            )
        )
    return findings


def compare_annotations(
    qt: qt_oracle.QtSnapshot, web: web_oracle.WebSnapshot, scenario: Scenario
) -> List[Finding]:
    """The distance a distance-sorted track advertises, against the distance Qt
    ordered it by. Only meaningful once a sort actually used station geometry."""
    if scenario.state["sort"] != "distance":
        return []
    findings: List[Finding] = []
    for index, nslc in enumerate(web.track_order):
        annotation = web.sort_annotations[index]
        subject = _nslc(nslc)
        if not annotation:
            findings.append(
                Finding(
                    kind="annotations",
                    subject=subject,
                    ok=False,
                    detail="a distance sort left the track unannotated",
                )
            )
            continue
        if annotation == "no coords":
            continue
        web_km = float(annotation.split()[0])
        geometry = qt.station_geometry.get((nslc[0], nslc[1]))
        if geometry is None or geometry["dist_m"] is None:
            findings.append(
                Finding(
                    kind="annotations",
                    subject=subject,
                    ok=False,
                    detail="Qt has no distance for this station",
                )
            )
            continue
        qt_km = float(geometry["dist_m"]) / 1000.0
        delta = abs(qt_km - web_km)
        # The label is printed to 0.1 km, so anything under half of that is a
        # rounding artefact rather than a disagreement about geometry.
        ok = delta <= 0.05
        findings.append(
            Finding(
                kind="annotations",
                subject=subject,
                ok=ok,
                detail="" if ok else f"Qt {qt_km:.3f} km vs web {web_km:.1f} km",
                metrics={"qt_km": qt_km, "web_km": web_km, "delta_km": delta},
            )
        )
    return findings


def compare_stability(run: web_oracle.WebRun) -> List[Finding]:
    """Drawing a view must be a pure function of the data, and must leave it alone.

    Two checks, because either can pass while the other fails: degapping an already
    degapped pile is idempotent, so a repeated request can return the identical
    payload over data that the first request quietly rewrote.
    """
    findings: List[Finding] = []
    if len(run.pile_before) != len(run.pile_after):
        # A degapper that merges or splits traces changes the count, and that is a
        # rewrite in itself — report it instead of tripping over an uneven zip.
        findings.append(
            Finding(
                kind="stability",
                subject="pile-immutability",
                ok=False,
                detail=(
                    f"computing the view changed the number of loaded traces: "
                    f"{len(run.pile_before)} before, {len(run.pile_after)} after"
                ),
                metrics={"before": len(run.pile_before), "after": len(run.pile_after)},
            )
        )
    elif run.pile_before != run.pile_after:
        changed = [
            f"{_nslc(before[0])} {before[3]}->{after[3]} samples"
            for before, after in zip(run.pile_before, run.pile_after, strict=True)
            if before != after
        ]
        findings.append(
            Finding(
                kind="stability",
                subject="pile-immutability",
                ok=False,
                detail="computing the view rewrote the loaded traces: " + ", ".join(changed[:4]),
                metrics={"changed_traces": len(changed)},
            )
        )
    else:
        findings.append(Finding(kind="stability", subject="pile-immutability", ok=True))

    first, second = run.first, run.second
    if first.raw_ys is None or second.raw_ys is None:
        return findings
    same_shape = first.raw_ys.shape == second.raw_ys.shape
    identical = same_shape and bool(np.array_equal(first.raw_ys, second.raw_ys))
    order_same = first.track_order == second.track_order
    ok = identical and order_same
    detail = ""
    if not ok:
        if not order_same:
            detail = "the second identical request produced a different track order"
        elif not same_shape:
            detail = (
                f"payload size changed between identical requests: "
                f"{first.raw_ys.size} then {second.raw_ys.size}"
            )
        else:
            delta = float(np.max(np.abs(first.raw_ys - second.raw_ys)))
            detail = f"payload changed between identical requests, max|Δ|={_fmt(delta)}"
    findings.append(Finding(kind="stability", subject="repeat-request", ok=ok, detail=detail))
    return findings


def compare_all(
    qt: qt_oracle.QtSnapshot,
    run: web_oracle.WebRun,
    scenario: Scenario,
) -> List[Finding]:
    web = run.first
    findings: List[Finding] = []
    findings.extend(compare_presence(qt, web))
    findings.extend(compare_samples(qt, web, scenario.compare))
    findings.extend(compare_tracks(qt, web))
    findings.extend(compare_scales(qt, web, scenario))
    findings.extend(compare_annotations(qt, web, scenario))
    findings.extend(compare_stability(run))
    return findings


def failing_kinds(findings: Iterable[Finding]) -> List[str]:
    return sorted({finding.kind for finding in findings if not finding.ok})
