"""Tier 3: the same scenario through both GUIs, one normalized snapshot each step.

Thin by design.
Only what genuinely needs a GUI lives here — menu and keyboard toggles, the control
strip, marker creation and editing — because anything that is really a question about
numbers belongs in tier 1, where it runs a thousand times faster and cannot flake.

Two things are deliberately *not* compared:

* **The view time range and the track order.** The web frontend paints them; it does
  not publish them to its accessibility layer, so there is nothing on that side to
  read. Extending the a11y layer to expose them is the natural way to widen tier 3.
* **Where a pick landed.** A fractional canvas position becomes a different time and a
  different track under the two layouts, and this suite never asserts on layout.
  The marker's *identity* — kind, phase, polarity, selection, and whether it is
  channel-scoped at all — is compared; its time and channel are recorded for a human.

    python tests/parity/run_scenarios.py          # needs the app served
"""

from __future__ import annotations

# harness first: it prepares the environment before the drivers it spawns need it.
import harness  # isort: skip

import argparse
import json
import subprocess
import sys
from typing import Any, Dict, List, Tuple

import compare
import gaps
from gaps import normalize_path
from harness import DIM, GREEN, HERE, RED, RESET, ROOT, YELLOW
from scenario import Scenario, load_all

ARTIFACTS = harness.ARTIFACTS
PLAN = ARTIFACTS / "tier3-plan.json"
QT_JSON = ARTIFACTS / "tier3-qt.json"
WEB_JSON = ARTIFACTS / "tier3-web.json"
SCREENSHOTS = ARTIFACTS / "screenshots" / "tier3"


def web_menu_labels() -> Dict[str, Tuple[str, str]]:
    """Normalized Qt menu path -> the (menu, item) labels the web actually shows.

    Built from the tier-2 web inventory rather than guessed, so the driver clicks the
    string the app renders and not an approximation of it.
    """
    inventory = json.loads((ARTIFACTS / "inventory-web.json").read_text())
    out: Dict[str, Tuple[str, str]] = {}
    for entry in inventory["menus"]:
        path = str(entry["path"])
        if "/" not in path:
            continue
        menu, item = path.split("/", 1)
        out[normalize_path(path)] = (menu, item)
    return out


def build_plan(scenarios: List[Scenario], register: gaps.Register) -> List[Dict[str, Any]]:
    labels = web_menu_labels()
    plans: List[Dict[str, Any]] = []
    for scenario in scenarios:
        steps: List[Dict[str, Any]] = []
        for index, step in enumerate(scenario.steps):
            label = f"{index}:{step.action}:{step.target or step.action}"
            entry: Dict[str, Any] = {"action": step.action, "label": label}
            if step.action == "menu":
                key = normalize_path(step.target)
                key = register.aliases.get(key, key)
                if key not in labels:
                    raise SystemExit(
                        f"{scenario.name}: no web menu item matches {step.target!r}; "
                        "add an [[alias]] to known_gaps.toml"
                    )
                menu, item = labels[key]
                entry.update(menu=menu, item=item)
            elif step.action == "key":
                entry["key"] = step.target
            elif step.action == "filter":
                entry.update(highpass=step.highpass, lowpass=step.lowpass)
            elif step.action == "pick":
                entry.update(x=step.x, y=step.y)
            steps.append(entry)
        plans.append({"name": scenario.name, "watchMenus": scenario.watch_menus, "steps": steps})
    return plans


def normalized_menu_state(state: Dict[str, bool]) -> Dict[str, bool]:
    return {normalize_path(key): value for key, value in state.items()}


def marker_identity(marker: Dict[str, Any]) -> Tuple[Any, ...]:
    """What a marker *is*, with where it was placed left out — see the module docstring.

    Selection is part of it, per marker: both sides publish it on the row itself, so
    "the right one is selected" is checked rather than only "as many are selected".
    """
    return (
        int(marker.get("kind", 0)),
        str(marker.get("phase") or ""),
        marker.get("polarity"),
        bool(marker.get("selected", False)),
        # Scoped-or-not, without which channel: the channel is where the pick
        # landed, and that differs between the layouts by construction. That a
        # double-click pick is bound to *a* track is placement-independent, and
        # is exactly what an unscoped-pick regression would flip.
        bool(marker.get("nslc")),
    )


def compare_step(
    qt: Dict[str, Any], web: Dict[str, Any], aliases: gaps.AliasMap
) -> List[compare.Finding]:
    findings: List[compare.Finding] = []
    label = qt["step"]

    qt_menus = normalized_menu_state(qt["menu_state"])
    web_menus = normalized_menu_state(web["menu_state"])
    mismatched = []
    unobserved = []
    for key, value in qt_menus.items():
        target = aliases.get(key, key)
        if target not in web_menus:
            unobserved.append(key)
        elif web_menus[target] != value:
            mismatched.append(f"{key}: Qt {value}, web {web_menus[target]}")
    findings.append(
        compare.Finding(
            kind="menu_state",
            subject=label,
            ok=not mismatched,
            detail="; ".join(mismatched),
            metrics={"compared": len(qt_menus) - len(unobserved), "unobserved": unobserved},
        )
    )

    control_diffs = []
    for name, qt_value in qt["controls"].items():
        web_value = web["controls"].get(name)
        if qt_value is None or web_value is None:
            if qt_value is not None or web_value is not None:
                control_diffs.append(f"{name}: Qt {qt_value}, web {web_value}")
            continue
        # The web reads its value back out of a text field that rounds for display, so
        # compare at the precision the field actually shows.
        if abs(float(qt_value) - float(web_value)) > 5e-3 * max(1.0, abs(float(qt_value))):
            control_diffs.append(f"{name}: Qt {qt_value}, web {web_value}")
    findings.append(
        compare.Finding(
            kind="controls",
            subject=label,
            ok=not control_diffs,
            detail="; ".join(control_diffs),
            metrics={"qt": qt["controls"], "web": web["controls"]},
        )
    )

    qt_markers = sorted(marker_identity(marker) for marker in qt["markers"])
    web_markers = sorted(marker_identity(marker) for marker in web["markers"])
    qt_selected = int(qt.get("selected_count", 0))
    web_selected = int(web.get("selected_count", 0))
    same = qt_markers == web_markers and qt_selected == web_selected
    detail = ""
    if qt_markers != web_markers:
        detail = f"Qt {len(qt_markers)} {qt_markers} vs web {len(web_markers)} {web_markers}"
    elif qt_selected != web_selected:
        detail = f"{qt_selected} selected in Qt, {web_selected} in the web"
    findings.append(
        compare.Finding(
            kind="markers",
            subject=label,
            ok=same,
            detail=detail,
            metrics={
                "qt_count": len(qt_markers),
                "web_count": len(web_markers),
                "qt_selected": qt_selected,
                "web_selected": web_selected,
                "qt_placement": [
                    (marker.get("time"), marker.get("nslc")) for marker in qt["markers"]
                ],
                "web_placement": [
                    (marker.get("time"), marker.get("nslc")) for marker in web["markers"]
                ],
            },
        )
    )
    return findings


def run(args: argparse.Namespace) -> int:
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    scenarios = load_all(tier=3, only=args.scenario or None)
    if not scenarios:
        raise SystemExit("no tier-3 scenarios matched")
    register = gaps.Register.load()

    if not (ARTIFACTS / "inventory-web.json").exists():
        subprocess.run(
            ["bun", str(HERE / "inventory_web.ts"), "--out", str(ARTIFACTS / "inventory-web.json")],
            check=True,
            cwd=ROOT,
        )

    PLAN.write_text(json.dumps(build_plan(scenarios, register), indent=2) + "\n")

    if not args.no_qt:
        subprocess.run(
            [
                sys.executable,
                str(HERE / "scenario_qt.py"),
                "--out",
                str(QT_JSON),
                "--screenshots",
                str(SCREENSHOTS),
            ]
            + [scenario.name for scenario in scenarios],
            check=True,
            cwd=ROOT,
        )
    if not args.no_web:
        subprocess.run(
            [
                "bun",
                str(HERE / "scenario_web.ts"),
                "--plan",
                str(PLAN),
                "--out",
                str(WEB_JSON),
                "--screenshots",
                str(SCREENSHOTS),
            ],
            check=True,
            cwd=ROOT,
        )

    qt_runs = json.loads(QT_JSON.read_text())
    web_runs = json.loads(WEB_JSON.read_text())

    report: List[Dict[str, Any]] = []
    failed: List[str] = []
    defects: Dict[str, List[str]] = {}

    for scenario in scenarios:
        qt_steps = qt_runs.get(scenario.name, [])
        web_steps = web_runs.get(scenario.name, [])
        if len(qt_steps) != len(web_steps):
            print(
                f"{RED}FAIL{RESET} {scenario.name:<26} "
                f"the two drivers produced {len(qt_steps)} and {len(web_steps)} snapshots"
            )
            failed.append(scenario.name)
            continue

        findings: List[compare.Finding] = []
        for qt_step, web_step in zip(qt_steps, web_steps, strict=True):
            findings.extend(compare_step(qt_step, web_step, register.aliases))
        problems, hit = gaps.evaluate(scenario.compare.expect_divergence, findings, register)

        for key in hit:
            defects.setdefault(key, []).append(scenario.name)
        status = (
            f"{RED}FAIL{RESET}"
            if problems
            else (f"{YELLOW}bug {RESET}" if hit else f"{GREEN}pass{RESET}")
        )
        passed = sum(1 for f in findings if f.ok)
        print(
            f"{status} {scenario.name:<26} {passed}/{len(findings)} checks match "
            f"over {len(qt_steps)} steps"
        )
        for problem in problems:
            print(f"       {RED}{problem}{RESET}")
        if args.verbose:
            for finding in findings:
                if not finding.ok or args.verbose > 1:
                    print(
                        f"       {'ok  ' if finding.ok else 'FAIL'} {finding.label()} {finding.detail}"
                    )
        if problems:
            failed.append(scenario.name)
        report.append(
            {
                "scenario": scenario.name,
                "problems": problems,
                "defects": hit,
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

    (ARTIFACTS / "tier3.json").write_text(json.dumps(report, indent=2, default=str) + "\n")
    print()
    if defects:
        print(f"{YELLOW}confirmed defects still reproducing ({len(defects)}):{RESET}")
        for key, names in sorted(defects.items()):
            print(
                f"  {YELLOW}{key}{RESET} — {register.entries[key]['title']}"
                f"  {DIM}({', '.join(names)}){RESET}"
            )
        print()
    if failed:
        print(f"{RED}{len(failed)}/{len(scenarios)} scenarios failed: {', '.join(failed)}{RESET}")
        return 1
    print(f"{GREEN}{len(scenarios)}/{len(scenarios)} scenarios behave as documented{RESET}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", nargs="*")
    parser.add_argument("--no-qt", action="store_true", help="reuse the last Qt snapshots")
    parser.add_argument("--no-web", action="store_true", help="reuse the last web snapshots")
    parser.add_argument("-v", "--verbose", action="count", default=0)
    return run(parser.parse_args())


if __name__ == "__main__":
    sys.exit(main())
