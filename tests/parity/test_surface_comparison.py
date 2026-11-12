"""The widened checks must reject missing capabilities and incorrect cross-marker state."""

from __future__ import annotations

import copy

import gaps
import pytest
from run_inventory import compare_capabilities, diff_section, parameter_entries
from run_scenarios import compare_step


def test_choices_and_bounds_are_enforced_without_rejecting_web_extensions() -> None:
    aliases = gaps.AliasMap({})
    qt = [{"name": "Corner", "minimum": 0.1, "maximum": 25.0, "choices": ["a", "b"]}]
    web = [{"name": "Corner", "minimum": 0.01, "maximum": 50.0, "choices": ["a", "b", "c"]}]
    result = diff_section("controls", qt, web, "name", aliases)
    compare_capabilities(result, aliases)
    assert result["missing"] == []
    web[0].update(minimum=1.0, maximum=20.0, choices=["a"])
    result = diff_section("controls", qt, web, "name", aliases)
    compare_capabilities(result, aliases)
    assert set(result["missing"]) == {"corner/b", "corner/minimum", "corner/maximum"}


def test_scoped_parameter_aliases_do_not_hide_another_panels_missing_parameter() -> None:
    qt = {
        "snufflings": [{"name": name, "parameters": [{"name": "Seconds"}]} for name in ["A", "B"]]
    }
    web = {
        "snufflings": [{"name": name, "parameters": [{"name": "Duration"}]} for name in ["A", "B"]]
    }
    aliases = gaps.AliasMap(
        {"snufflings/run snuffling/a/seconds": "snufflings/run snuffling/a/duration"}
    )
    result = diff_section(
        "parameters", parameter_entries(qt), parameter_entries(web), "path", aliases
    )
    assert result["missing"] == ["snufflings/run snuffling/b/seconds"]


def test_display_unit_conversion_still_rejects_a_narrower_range() -> None:
    aliases = gaps.AliasMap({"length": "length [km]"}, {"length": 0.001})
    qt = [{"name": "Length", "minimum": 100.0, "maximum": 100000.0}]
    web = [{"name": "Length [km]", "minimum": 0.1, "maximum": 100.0}]
    result = diff_section("parameters", qt, web, "name", aliases)
    compare_capabilities(result, aliases)
    assert result["missing"] == []
    web[0]["maximum"] = 50.0
    result = diff_section("parameters", qt, web, "name", aliases)
    compare_capabilities(result, aliases)
    assert result["missing"] == ["length/maximum"]


def test_lowercase_cake_phase_does_not_stand_in_for_uppercase_phase() -> None:
    qt = {
        "snufflings": [
            {
                "name": "Cake Phase",
                "parameters": [
                    {"name": phase, "ident": f"wantphase_{index}"}
                    for index, phase in enumerate(["p", "P"])
                ],
            }
        ]
    }
    web = {
        "snufflings": [{"name": "Cake Phase", "parameters": [{"name": "p", "role": "checkbox"}]}]
    }
    result = diff_section(
        "parameters", parameter_entries(qt), parameter_entries(web), "path", gaps.AliasMap({})
    )
    assert result["missing"] == ["snufflings/run snuffling/cake phase/P"]


@pytest.mark.parametrize("mutation", ["window", "active"])
def test_scenario_rejects_wrong_window_or_wrong_active_event(mutation: str) -> None:
    qt = {
        "step": "event",
        "window": [1700000000.0, 1700000100.0],
        "controls": {},
        "menu_state": {},
        "markers": [
            {"kind": 1, "is_event": True, "origin_time": 100.0, "active": True},
            {"kind": 1, "is_event": True, "origin_time": 200.0, "active": False},
        ],
    }
    web = copy.deepcopy(qt)
    assert all(finding.ok for finding in compare_step(qt, web, gaps.AliasMap({})))
    if mutation == "window":
        web["window"][0] += 1.0
    else:
        for marker in web["markers"]:
            marker["active"] = not marker["active"]
    assert any(not finding.ok for finding in compare_step(qt, web, gaps.AliasMap({})))
