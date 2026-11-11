"""Snuffler's ':' command line: hide, unhide, quick filters, scaling and goto."""

from __future__ import annotations

import re
from typing import List, Optional

from snuffler_web_proto import data_pb2

from . import events, state


def _command_glob(scope: Optional[str], pattern: str) -> str:
    """Build an "n.s.l.c" fnmatch glob from an optional scope + pattern."""
    idx = {"n": 0, "s": 1, "l": 2, "c": 3}
    if scope in idx:
        fields = ["*", "*", "*", "*"]
        fields[idx[scope]] = pattern
        return ".".join(fields)
    # No scope: the token is the glob itself, anchored over the full
    # "net.sta.loc.cha" string as snuffler passes it to `match_nslc`
    # (pile_viewer.py:4326).
    return pattern


def _visibility_patterns(args: List[str]) -> List[str]:
    """The globs a ``hide``/``unhide`` line addresses, read as ``inputline_finished`` reads them.

    A lone argument is the pattern itself, while a leading ``n|s|l|c`` scopes
    every argument after it (``hide n GE GR`` hides two networks).
    Anything else addresses nothing.
    """
    match args:
        case [pattern]:
            return [_command_glob(None, pattern)]
        case [("n" | "s" | "l" | "c") as scope, *rest] if rest:
            return [_command_glob(scope, tok) for tok in rest]
        case _:
            return []


def _set_hidden(hide: bool, globs: List[str]) -> data_pb2.CommandResult:
    """Add the globs to the blacklist, or take them off it.

    Unhiding a pattern that is not blacklisted is an error, as it is in snuffler
    (``remove_blacklist_pattern``).
    """
    for glob in globs:
        if hide:
            _hide_pattern(glob)
        elif glob in state.hidden_patterns:
            state.hidden_patterns = [h for h in state.hidden_patterns if h != glob]
        else:
            # Whatever came off the blacklist before the unknown pattern stays
            # off it, so the partially-updated scene rides along with the error.
            return _error_result("Pattern not found in blacklist.", with_scene=True)
    return _scene_result()


def _set_scaling(pattern: Optional[str], vmin_tok: str, vmax_tok: str) -> data_pb2.CommandResult:
    """The ':scaling' command: pin the amplitude range of the tracks matching ``pattern``.

    A ``pattern`` of None addresses every track.
    Bounds are read with the same ``float_or_none`` snuffler uses, so 'nan'
    leaves that bound automatic.
    """
    from pyrocko.model import float_or_none

    try:
        vmin, vmax = float_or_none(vmin_tok), float_or_none(vmax_tok)
    except ValueError:
        return _error_result("scaling: vmin/vmax must be numbers or 'nan'")
    state.scaling_hooks["_" if pattern is None else pattern] = (vmin, vmax)
    return _scene_result()


def _empty_channel_patterns(tmin: float, tmax: float) -> List[str]:
    """The exact "n.s.l.c" globs of channels with no data in the visible window,
    for snuffler's ``hide empty`` (``add_blacklist_pattern('empty')``)."""
    have_data = set()
    for trace in state.traces:
        if trace.tmax >= tmin and trace.tmin <= tmax:
            have_data.add(trace.nslc_id)
    empty = {trace.nslc_id for trace in state.traces} - have_data
    return sorted(".".join(nslc) for nslc in empty)


def _hide_pattern(glob: str) -> None:
    """Blacklist a glob, moving an already-listed one to the end (snuffler's
    ``add_blacklist_pattern``)."""
    state.hidden_patterns = [h for h in state.hidden_patterns if h != glob]
    state.hidden_patterns.append(glob)


def _scene_result() -> data_pb2.CommandResult:
    result = data_pb2.CommandResult(action="scene")
    result.scene.CopyFrom(state.scene_meta())
    return result


def _error_result(message: str, with_scene: bool = False) -> data_pb2.CommandResult:
    """A rejected command.

    ``with_scene`` carries the scene along for a command that changed what is
    shown before it failed.
    """
    result = data_pb2.CommandResult(action="error", message=message)
    if with_scene:
        result.scene.CopyFrom(state.scene_meta())
    return result


def _goto_window_length_s(spec: str) -> Optional[float]:
    """The visible window (seconds) matching how specific a ``goto`` date is
    (snuffler): a bare year → ~1yr, year-month → ~1 month, a full date → 1 day,
    +hour → 1h, +minute → 1min; a fully-specified time keeps the current window."""
    s = spec.strip()
    if re.fullmatch(r"\d{4}", s):
        return 12 * 32 * 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}", s):
        return 32 * 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}", s):
        return 24 * 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}[ tT]\d{1,2}", s):
        return 3600.0
    if re.fullmatch(r"\d{4}-\d{1,2}-\d{1,2}[ tT]\d{1,2}:\d{1,2}", s):
        return 60.0
    return None


def goto_result(args: List[str], tmin: float, tmax: float) -> data_pb2.CommandResult:
    """Snuffler's ``goto`` arm: a date, a bare time of day (taken on the date at
    the centre of the visible window), or the name of a loaded event."""
    if not args:
        return _error_result("usage: goto <time>")
    spec = " ".join(args).strip()
    from pyrocko import util

    # A bare "HH:MM[:SS[.XXX]]" means that time on the day being looked at.
    if re.fullmatch(r"\d{1,2}:\d{2}(:\d{2}(\.\d+)?)?", spec):
        day = util.time_to_str((tmin + tmax) * 0.5, format="%Y-%m-%d")
        try:
            return data_pb2.CommandResult(
                action="goto", time=float(util.str_to_time_fillup("%s %s" % (day, spec)))
            )
        except Exception:
            return _error_result("goto: cannot read time: %s" % spec)

    try:
        result = data_pb2.CommandResult(action="goto", time=float(util.str_to_time_fillup(spec)))
    except Exception:
        pass
    else:
        window_length_s = _goto_window_length_s(spec)
        if window_length_s is not None:
            result.window_length_s = window_length_s
        elif spec.lower() in ("today", "yesterday", "tomorrow"):
            result.window_length_s = 24 * 3600.0
        return result

    # Not a time — fall back to a loaded event by name (snuffler
    # go_to_event_by_name): jump the view to that event's origin time.
    target = spec.lower()
    for h, event in state.event_registry.items():
        names = {
            (event.name or "").lower(),
            events.short_event_name(event.name).lower(),
            h.lower(),
        }
        if target in names:
            return data_pb2.CommandResult(action="goto", time=float(event.time))
    return _error_result("no event or time matching: %s" % spec)


def run(request: data_pb2.CommandRequest) -> data_pb2.CommandResult:
    """Execute a snuffler ':' command line into a CommandResult whose ``action``
    is ``goto`` (with ``t``), ``scene`` (with ``scene``), ``error`` (with
    ``message``), or ``none``.

    Supported, matching snuffler's ``inputline_finished``: ``n|s|l|c [<pattern>...]``
    (quick filter); ``hide <pattern>`` / ``hide n|s|l|c <tok>...`` (also
    ``hide empty``); ``unhide [<pattern>]``; ``scaling [<pattern>] <vmin> <vmax>``;
    ``goto <time>|<eventname>``.
    ``markers`` never reaches Python: marker visibility is Rust-owned state,
    handled there.
    """
    parts = request.line.split()
    if not parts:
        return data_pb2.CommandResult(action="none")
    cmd, args = parts[0].lower(), parts[1:]

    match cmd, args:
        case "goto", spec:
            return goto_result(spec, request.visible_tmin, request.visible_tmax)

        # The quick filter: several patterns are OR'd ("c HH? BH?"), and a command
        # with no pattern at all turns the filter off again.
        case ("n" | "s" | "l" | "c") as scope, patterns:
            state.quick_filter_patterns = (
                [_command_glob(scope, pat) for pat in patterns] if patterns else None
            )
            return _scene_result()

        case "unhide", []:
            state.hidden_patterns = []
            return _scene_result()

        case "hide", ["empty"]:
            for empty in _empty_channel_patterns(request.visible_tmin, request.visible_tmax):
                _hide_pattern(empty)
            return _scene_result()

        case ("hide" | "unhide") as verb, _:
            return _set_hidden(verb == "hide", _visibility_patterns(args))

        # snuffler's 'scaling' arms, by argument count: none clears every rule,
        # one is a usage error, two set the global rule, three set a per-pattern
        # rule, and more are silently ignored.
        case "scaling", []:
            state.scaling_hooks.clear()
            return _scene_result()
        case "scaling", [_]:
            return _error_result("usage: scaling [n.s.l.c] vmin vmax")
        case "scaling", [vmin, vmax]:
            return _set_scaling(None, vmin, vmax)
        case "scaling", [pattern, vmin, vmax]:
            return _set_scaling(pattern, vmin, vmax)
        case "scaling", _:
            return _scene_result()

        case _:
            return _error_result("No such command: %s" % cmd)
