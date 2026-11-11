"""Unit tests for the register's own enforcement.

The parity tiers trust ``gaps.evaluate`` to hold findings against
``known_gaps.toml`` in both directions, and ``gaps.Register.load`` to reject a
malformed register before anything runs. That machinery is the one place a bug
would not show up as a failing scenario — it would show up as a scenario passing
for the wrong reason — so its edge cases are pinned here instead of being
exercised only by whichever divergences the corpus happens to produce.

    task test:parity:unit
"""

from __future__ import annotations

import textwrap
from pathlib import Path
from typing import Any, Dict, Optional

import compare
import gaps
import pytest


def finding(
    kind: str,
    subject: str = "",
    ok: bool = False,
    metrics: Optional[Dict[str, Any]] = None,
) -> compare.Finding:
    return compare.Finding(
        kind=kind, subject=subject, ok=ok, detail="synthetic", metrics=metrics or {}
    )


def load_register(tmp_path: Path, body: str) -> gaps.Register:
    path = tmp_path / "known_gaps.toml"
    path.write_text(textwrap.dedent(body))
    return gaps.Register.load(path)


# One entry with two kinds scoped to the channel the divergence lives on, and one
# kind (with a structural subject) left kind-wide.
SCOPED = """
    [[numeric]]
    key = "scoped"
    checks = ["presence", "geometry", "scales"]
    subjects.geometry = ["XR.ODD3.*"]
    subjects.scales = ["XR.ODD3.*"]
    title = "scoped divergence"
    why = "test register"
"""

# The full set of failures the scoped entry documents, all inside its scope.
SCOPED_FAILURES = [
    finding("presence"),
    finding("geometry", "XR.ODD3..BHZ"),
    finding("scales", "XR.ODD3..BHZ"),
]


def test_covered_failures_with_every_kind_reproducing_pass_clean(tmp_path: Path) -> None:
    register = load_register(tmp_path, SCOPED)
    problems, defects = gaps.evaluate(["scoped"], SCOPED_FAILURES, register)
    assert problems == []
    assert defects == []


def test_out_of_scope_failure_is_unexplained(tmp_path: Path) -> None:
    register = load_register(tmp_path, SCOPED)
    # A scales failure on a channel outside the entry's scope must fail even
    # though the entry covers the 'scales' kind — this is the whole point of
    # subject scoping.
    extra = [*SCOPED_FAILURES, finding("scales", "XR.ODD1..BHZ")]
    problems, _defects = gaps.evaluate(["scoped"], extra, register)
    assert len(problems) == 1
    assert "unexplained divergence in scales" in problems[0]
    assert "XR.ODD1..BHZ" in problems[0]


def test_kind_without_subjects_stays_kind_wide(tmp_path: Path) -> None:
    register = load_register(tmp_path, SCOPED)
    # 'presence' has no subjects declared, so its structural empty subject is
    # covered without matching any pattern.
    problems, _defects = gaps.evaluate(["scoped"], SCOPED_FAILURES, register)
    assert not any("presence" in problem for problem in problems)


def test_strict_entry_goes_stale_per_kind(tmp_path: Path) -> None:
    register = load_register(tmp_path, SCOPED)
    # geometry stops diverging while the other kinds still reproduce: exactly one
    # staleness problem, naming the kind and the entry.
    without_geometry = [f for f in SCOPED_FAILURES if f.kind != "geometry"]
    problems, _defects = gaps.evaluate(["scoped"], without_geometry, register)
    assert len(problems) == 1
    assert "geometry was expected to diverge and did not" in problems[0]
    assert "'scoped'" in problems[0]


def test_out_of_scope_failure_does_not_satisfy_strictness(tmp_path: Path) -> None:
    register = load_register(tmp_path, SCOPED)
    # geometry fails only outside the scope: that is both an unexplained
    # divergence and a stale in-scope expectation, and neither may absorb the
    # other.
    swapped = [
        finding("presence"),
        finding("geometry", "XR.ODD1..BHZ"),
        finding("scales", "XR.ODD3..BHZ"),
    ]
    problems, _defects = gaps.evaluate(["scoped"], swapped, register)
    assert any("unexplained divergence in geometry" in problem for problem in problems)
    assert any("geometry was expected to diverge and did not" in problem for problem in problems)


def test_non_strict_entry_may_stop_reproducing(tmp_path: Path) -> None:
    register = load_register(
        tmp_path,
        """
        [[numeric]]
        key = "sometimes"
        checks = ["scales"]
        strict = false
        title = "data-dependent divergence"
        why = "test register"
        """,
    )
    problems, _defects = gaps.evaluate(["sometimes"], [], register)
    assert problems == []


def test_unknown_expectation_key_is_a_problem(tmp_path: Path) -> None:
    register = load_register(tmp_path, SCOPED)
    problems, _defects = gaps.evaluate(["missing"], [], register)
    assert len(problems) == 1
    assert "known_gaps.toml lacks" in problems[0]


def test_defect_entries_excuse_and_report_their_hits(tmp_path: Path) -> None:
    register = load_register(
        tmp_path,
        """
        [[defect]]
        key = "bug"
        checks = ["markers"]
        title = "a registered bug"
        why = "test register"
        """,
    )
    problems, defects = gaps.evaluate(["bug"], [finding("markers", "step")], register)
    assert problems == []
    assert defects == ["bug"]


def test_uncovered_failures_aggregate_per_kind(tmp_path: Path) -> None:
    register = load_register(tmp_path, SCOPED)
    # Three uncovered failures of one kind report as a single problem carrying
    # the count, so a regression sweeping many tracks stays readable.
    sweep = [*SCOPED_FAILURES, *(finding("scales", f"XR.ODD1..BH{c}") for c in "ZNE")]
    problems, _defects = gaps.evaluate(["scoped"], sweep, register)
    assert len(problems) == 1
    assert "(3 findings)" in problems[0]


RESIDUAL = """
    [[numeric]]
    key = "const"
    checks = ["samples"]
    residual = "constant"
    title = "constant-offset divergence"
    why = "test register"

    [[numeric]]
    key = "blanket"
    checks = ["samples"]
    strict = false
    title = "unconditional excuse"
    why = "test register"
"""

CONSTANT_SHAPED = finding(
    "samples", "XX.AAA..HHZ", metrics={"max_abs_centered": 1e-12, "bound": 1e-9}
)
WRONG_SHAPED = finding("samples", "XX.AAA..HHZ", metrics={"max_abs_centered": 1e-6, "bound": 1e-9})


def test_residual_accepts_a_divergence_of_the_declared_shape(tmp_path: Path) -> None:
    register = load_register(tmp_path, RESIDUAL)
    problems, _defects = gaps.evaluate(["const"], [CONSTANT_SHAPED], register)
    assert problems == []


def test_residual_rejects_a_divergence_of_another_shape(tmp_path: Path) -> None:
    register = load_register(tmp_path, RESIDUAL)
    problems, _defects = gaps.evaluate(["const"], [WRONG_SHAPED], register)
    assert len(problems) == 1
    assert "not the constant offset" in problems[0]


def test_residual_rejects_a_finding_without_the_metric(tmp_path: Path) -> None:
    register = load_register(tmp_path, RESIDUAL)
    problems, _defects = gaps.evaluate(["const"], [finding("samples", "XX.AAA..HHZ")], register)
    assert len(problems) == 1
    assert "n/a" in problems[0]


def test_residual_shape_wins_over_an_unconditional_excuse(tmp_path: Path) -> None:
    register = load_register(tmp_path, RESIDUAL)
    # Naming a blanket entry alongside the shaped one must not soften the shape
    # check — the excuse stays as tight as the tightest claim that covers it.
    problems, _defects = gaps.evaluate(["const", "blanket"], [WRONG_SHAPED], register)
    assert any("not the constant offset" in problem for problem in problems)


def test_blanket_entry_alone_excuses_unconditionally(tmp_path: Path) -> None:
    register = load_register(tmp_path, RESIDUAL)
    problems, _defects = gaps.evaluate(["blanket"], [WRONG_SHAPED], register)
    assert problems == []


# Register.load has to reject a malformed register before anything trusts it.


def test_load_rejects_subjects_for_uncovered_kinds(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="scopes subjects for kinds"):
        load_register(
            tmp_path,
            """
            [[numeric]]
            key = "bad"
            checks = ["scales"]
            subjects.geometry = ["XR.*"]
            title = "t"
            why = "w"
            """,
        )


def test_load_rejects_non_list_subject_patterns(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="non-empty list of glob patterns"):
        load_register(
            tmp_path,
            """
            [[numeric]]
            key = "bad"
            checks = ["scales"]
            subjects.scales = "XR.*"
            title = "t"
            why = "w"
            """,
        )


def test_load_rejects_unknown_check_kinds(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="check kinds that do not exist"):
        load_register(
            tmp_path,
            """
            [[numeric]]
            key = "bad"
            checks = ["waveform"]
            title = "t"
            why = "w"
            """,
        )


def test_load_rejects_duplicate_keys_across_sections(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="duplicate key"):
        load_register(
            tmp_path,
            """
            [[numeric]]
            key = "twice"
            checks = ["scales"]
            title = "t"
            why = "w"

            [[defect]]
            key = "twice"
            checks = ["markers"]
            title = "t"
            why = "w"
            """,
        )


def test_load_rejects_a_residual_without_the_samples_check(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="does not\\s+cover the 'samples' check"):
        load_register(
            tmp_path,
            """
            [[numeric]]
            key = "bad"
            checks = ["scales"]
            residual = "constant"
            title = "t"
            why = "w"
            """,
        )


def test_load_rejects_an_unknown_residual_model(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="unknown residual model"):
        load_register(
            tmp_path,
            """
            [[numeric]]
            key = "bad"
            checks = ["samples"]
            residual = "linear"
            title = "t"
            why = "w"
            """,
        )


def test_load_rejects_incomplete_manual_entries(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="need key, title and why"):
        load_register(
            tmp_path,
            """
            [[manual]]
            key = "undocumented"
            title = "t"
            """,
        )


def test_load_rejects_manual_entries_that_name_no_enforcing_check(tmp_path: Path) -> None:
    # The kind's promise is that an unobservable difference still has a way out of
    # that state, so an entry that cannot name one is rejected.
    with pytest.raises(SystemExit, match="must say in `enforce`"):
        load_register(
            tmp_path,
            """
            [[manual]]
            key = "unobservable"
            title = "t"
            why = "w"
            """,
        )


def test_load_rejects_duplicate_manual_keys(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="duplicate key"):
        load_register(
            tmp_path,
            """
            [[manual]]
            key = "twice"
            title = "t"
            why = "w"
            enforce = "e"

            [[manual]]
            key = "twice"
            title = "t"
            why = "w"
            enforce = "e"
            """,
        )


def test_unreferenced_names_entries_no_scenario_declares(tmp_path: Path) -> None:
    register = load_register(
        tmp_path,
        """
        [[numeric]]
        key = "named"
        checks = ["scales"]
        title = "t"
        why = "w"

        [[defect]]
        key = "orphan"
        checks = ["samples"]
        title = "t"
        why = "w"
        """,
    )
    # An entry no scenario names is never passed to evaluate, so it can neither
    # excuse nor go stale — the state [[manual]] exists to hold instead.
    assert register.unreferenced(["named"]) == ["orphan"]
    assert register.unreferenced(["named", "orphan"]) == []


def test_load_rejects_manual_keys_that_shadow_enforced_entries(tmp_path: Path) -> None:
    with pytest.raises(SystemExit, match="duplicate key"):
        load_register(
            tmp_path,
            """
            [[numeric]]
            key = "twice"
            checks = ["scales"]
            title = "t"
            why = "w"

            [[manual]]
            key = "twice"
            title = "t"
            why = "w"
            """,
        )
