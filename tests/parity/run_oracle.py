"""Tier 1: oracle parity. Qt snuffler and web/bridge.py, same inputs, same process.

Runs every tier-1 scenario through both implementations and diffs the numbers. No
browser, no server, no VNC, no Docker — an offscreen ``QApplication`` on one side
and a direct ``import bridge`` on the other.

A scenario passes when every check either matches or diverges in exactly the way
``known_gaps.toml`` says it should. A divergence nobody wrote down fails the run,
and so does a documented divergence that stopped happening — the second is how the
allowlist avoids rotting into a list of things that used to be true.

    QT_QPA_PLATFORM=offscreen PYTHONPATH=web/proto:web python tests/parity/run_oracle.py
"""

from __future__ import annotations

# harness first: it prepares the environment before pyrocko or the bridge load.
import harness  # isort: skip

import argparse
import json
import sys
import traceback
from typing import Any, Dict, List

import compare
import freq_limits
import gaps
import qt_oracle
import web_oracle
from corpus import build as corpus_build
from harness import DIM, GREEN, RED, RESET, YELLOW
from scenario import DATASETS, CorpusFiles, Scenario, load_all

ARTIFACTS = harness.ARTIFACTS

# The one function pair with a written-down contract gets a property grid rather
# than a scenario; it runs under this name so it can be selected like one.
GRID_NAME = "decon-freq-limits"


def corpus_for(scenario: Scenario) -> CorpusFiles:
    dataset = DATASETS[scenario.dataset]
    return CorpusFiles(
        waveforms=corpus_build.path(dataset.waveforms),
        stations=corpus_build.path(dataset.stations) if dataset.stations else None,
        events=corpus_build.path(dataset.events) if dataset.events else None,
        responses=tuple(corpus_build.path(rel) for rel in dataset.responses),
    )


def summarize(findings: List[compare.Finding]) -> str:
    passed = sum(1 for f in findings if f.ok)
    return f"{passed}/{len(findings)} checks match"


def worst_metrics(findings: List[compare.Finding]) -> Dict[str, float]:
    """The headline numbers: how far apart the two sides actually were."""
    out: Dict[str, float] = {}
    rel = [f.metrics["rel_rms"] for f in findings if f.kind == "samples" and "rel_rms" in f.metrics]
    if rel:
        out["max_rel_rms"] = max(rel)
    scales = [
        max(f.metrics["rel_ymin"], f.metrics["rel_ymax"])
        for f in findings
        if f.kind == "scales" and "rel_ymin" in f.metrics
    ]
    if scales:
        out["max_rel_scale"] = max(scales)
    return out


def run_grid() -> bool:
    """Run the ``_decon_freq_limits`` property grid; return True when it agrees."""
    probe = qt_oracle.QtViewer([corpus_build.path("oddrate.mseed")])
    try:
        points, failures = freq_limits.run(probe.view)
    finally:
        probe.close()
    status = f"{GREEN}pass{RESET}" if not failures else f"{RED}FAIL{RESET}"
    print(f"{status} {GRID_NAME:<34} {points - len(failures)}/{points} grid points agree")
    for failure in failures[:10]:
        print(f"       {RED}{failure}{RESET}")
    return not failures


def run_scenario(
    scenario: Scenario, register: gaps.Register, want_png: bool
) -> tuple[qt_oracle.QtSnapshot, List[compare.Finding], List[str], List[str]]:
    """One scenario through both oracles; returns (qt, findings, problems, defects)."""
    files = corpus_for(scenario)
    qt = qt_oracle.run(scenario, files, want_png=want_png)
    web_run = web_oracle.run(scenario, files)
    findings = compare.compare_all(qt, web_run, scenario)
    problems, defects_hit = gaps.evaluate(scenario.compare.expect_divergence, findings, register)
    if qt.png:
        (ARTIFACTS / f"{scenario.name}.qt.png").write_bytes(qt.png)
    return qt, findings, problems, defects_hit


def run(args: argparse.Namespace) -> int:
    corpus_build.verify()
    register = gaps.Register.load()

    # An explicit selection names exactly what runs; the grid is selectable under
    # its own name and otherwise runs only alongside the full suite.
    selection = [name for name in (args.scenario or []) if name != GRID_NAME]
    want_grid = not args.scenario or GRID_NAME in args.scenario
    scenarios = load_all(tier=1, only=selection or None) if selection or not args.scenario else []
    if not scenarios and not want_grid:
        raise SystemExit("no tier-1 scenarios matched")

    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    report: List[Dict[str, Any]] = []
    failed: List[str] = []
    defects: Dict[str, List[str]] = {}
    total = len(scenarios)

    if want_grid:
        total += 1
        if not run_grid():
            failed.append(GRID_NAME)

    for scenario in scenarios:
        # One scenario crashing (a pyrocko pin bump renaming a menu item, a corpus
        # dataset misloading) must not take the rest of the evidence with it.
        try:
            qt, findings, problems, defects_hit = run_scenario(scenario, register, args.png)
        except Exception:
            print(f"{RED}FAIL{RESET} {scenario.name:<34} the harness itself failed:")
            print("       " + traceback.format_exc().replace("\n", "\n       ").rstrip())
            failed.append(scenario.name)
            report.append({"scenario": scenario.name, "error": traceback.format_exc()})
            continue

        for key in defects_hit:
            defects.setdefault(key, []).append(scenario.name)

        if problems:
            status = f"{RED}FAIL{RESET}"
        elif defects_hit:
            status = f"{YELLOW}bug {RESET}"
        else:
            status = f"{GREEN}pass{RESET}"
        metrics = worst_metrics(findings)
        trailer = " ".join(f"{k}={v:.2e}" for k, v in metrics.items())
        note = (
            f" {YELLOW}[{', '.join(scenario.compare.expect_divergence)}]{RESET}"
            if scenario.compare.expect_divergence
            else ""
        )
        print(f"{status} {scenario.name:<34} {summarize(findings)}{note}  {DIM}{trailer}{RESET}")
        for problem in problems:
            print(f"       {RED}{problem}{RESET}")
        if args.verbose:
            for finding in findings:
                if not finding.ok or args.verbose > 1:
                    mark = "ok  " if finding.ok else "FAIL"
                    print(f"       {mark} {finding.label()} {finding.detail}")

        if problems:
            failed.append(scenario.name)
        report.append(
            {
                "scenario": scenario.name,
                "title": scenario.title,
                "dataset": scenario.dataset,
                "window": list(qt.window),
                "state": scenario.state,
                "expect_divergence": scenario.compare.expect_divergence,
                "defects": defects_hit,
                "problems": problems,
                "metrics": metrics,
                "findings": [
                    {
                        "kind": f.kind,
                        "subject": f.subject,
                        "ok": f.ok,
                        "detail": f.detail,
                        "metrics": f.metrics,
                    }
                    for f in findings
                ],
            }
        )

    (ARTIFACTS / "tier1.json").write_text(json.dumps(report, indent=2, default=str) + "\n")
    print()
    # Only a full run knows what every scenario declares, so only a full run can
    # tell an inert entry from one this invocation simply did not reach.
    if not args.scenario:
        declared = {key for one in load_all() for key in one.compare.expect_divergence}
        orphans = register.unreferenced(declared)
        if orphans:
            print(f"{RED}{len(orphans)} enforced entries no scenario names:{RESET}")
            for key in orphans:
                print(f"  {RED}{key}{RESET} — {register.entries[key]['title']}")
            print(
                f"  {DIM}nothing excuses or checks these; give each a scenario or "
                f"move it to [[manual]]{RESET}"
            )
            print()
            failed.append("known_gaps.toml")
            total += 1
    if defects:
        print(f"{YELLOW}confirmed defects still reproducing ({len(defects)}):{RESET}")
        for key, names in sorted(defects.items()):
            print(
                f"  {YELLOW}{key}{RESET} — {register.entries[key]['title']}"
                f"  {DIM}({', '.join(names)}){RESET}"
            )
        print(f"  {DIM}see tests/parity/known_gaps.toml [[defect]] for each fix{RESET}")
        print()
    if failed:
        print(f"{RED}{len(failed)}/{total} failed: {', '.join(failed)}{RESET}")
        return 1
    print(f"{GREEN}{total}/{total} behave as documented{RESET}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "scenario",
        nargs="*",
        help=f"scenario names, or {GRID_NAME!r} for the property grid (default: everything)",
    )
    parser.add_argument("-v", "--verbose", action="count", default=0)
    parser.add_argument(
        "--png",
        action="store_true",
        help="also write the offscreen Qt render of each scenario, for human review",
    )
    return run(parser.parse_args())


if __name__ == "__main__":
    sys.exit(main())
