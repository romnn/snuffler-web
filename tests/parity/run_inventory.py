"""Tier 2: diff the two command surfaces and hold the difference to an allowlist.

Both halves are *extracted*, never listed by hand — :mod:`inventory_qt` off a live
offscreen widget tree, ``inventory_web.ts`` off the app's accessibility layer — so
the answer to "does the web port have all the features and modes" updates itself when
either side moves. A pinned pyrocko means the Qt half only moves when the pin does,
and a pin bump then shows up here as a precise list of new things to implement or to
justify.

The rule is one-directional and deliberately so: anything Qt offers, the web does not,
and ``known_gaps.toml`` does not excuse, fails the build. Web-only entries are
reported but never fail — the port is allowed to be more than a copy.

    python tests/parity/run_inventory.py            # regenerate both, then diff
    python tests/parity/run_inventory.py --no-web   # reuse the last web inventory
"""

from __future__ import annotations

# harness first: it prepares the environment before the extractors it spawns need it.
import harness  # isort: skip

import argparse
import json
import re
import subprocess
import sys
from typing import Any, Dict, Iterable, List, Tuple

import gaps
from gaps import normalize_path
from harness import DIM, GREEN, HERE, RED, RESET, ROOT, YELLOW

ARTIFACTS = harness.ARTIFACTS
QT_JSON = ARTIFACTS / "inventory-qt.json"
WEB_JSON = ARTIFACTS / "inventory-web.json"
SCREENSHOTS = ARTIFACTS / "screenshots" / "tier2" / "inventory"


def regenerate(want_qt: bool, want_web: bool) -> None:
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    if want_qt:
        subprocess.run(
            [
                sys.executable,
                str(HERE / "inventory_qt.py"),
                "--out",
                str(QT_JSON),
                "--screenshots",
                str(SCREENSHOTS),
            ],
            check=True,
            cwd=ROOT,
        )
    if want_web:
        command = [
            "bun",
            str(HERE / "inventory_web.ts"),
            "--out",
            str(WEB_JSON),
            "--screenshots",
            str(SCREENSHOTS),
        ]
        # The web probe presses exactly the keys Qt turned out to bind, so the two
        # halves of the key diff are asking about the same set.
        if QT_JSON.exists():
            tokens = [entry["key"] for entry in json.loads(QT_JSON.read_text()).get("keys", [])]
            probe = ARTIFACTS / "probe-keys.json"
            probe.write_text(json.dumps(tokens))
            command += ["--probe-keys", str(probe)]
        subprocess.run(command, check=True, cwd=ROOT)


_RANGE = re.compile(r"^(.*?)\s*\.\.\.\s*(.*)$")


def expand_keys(rows: Iterable[Dict[str, str]]) -> Dict[str, str]:
    """Turn help-table rows into one entry per key.

    The tables collapse families — ``0 ... 7``, ``<f1> ... <f9>``, ``<up>, <down>`` —
    and comparing collapsed rows compares the *notation*, not the bindings: Qt writing
    ``0 ... 5`` where the web writes ``0 ... 7`` looks like two different rows when it
    is really two keys the web has and Qt's page forgot to mention. Expanded, the diff
    is per key, which is the thing that either exists or does not.
    """
    out: Dict[str, str] = {}
    for row in rows:
        description = row.get("description", "")
        for part in [piece.strip() for piece in row["key"].split(",")]:
            if not part:
                continue
            match = _RANGE.match(part)
            if match is None:
                out.setdefault(part, description)
                continue
            for token in _range_tokens(match.group(1), match.group(2)):
                out.setdefault(token, description)
    return out


def _range_tokens(low: str, high: str) -> List[str]:
    """Expand ``0 ... 7`` and ``<f1> ... <f9>``; anything else stays as written."""
    if low.isdigit() and high.isdigit():
        return [str(value) for value in range(int(low), int(high) + 1)]
    fkey = re.compile(r"^<f(\d+)>$")
    start, stop = fkey.match(low), fkey.match(high)
    if start and stop:
        return [f"<f{index}>" for index in range(int(start.group(1)), int(stop.group(1)) + 1)]
    return [low, high]


def diff_keys(qt: Dict[str, Any], web: Dict[str, Any], aliases: gaps.AliasMap) -> Dict[str, Any]:
    """Match Qt's live key bindings against what the web binds.

    Qt's side is what the widget answers to, not what its help page claims. The web's
    side is two sources at once, because neither alone is enough: the app's own help
    window says what it means to bind, and the behavioural probe says what actually did
    something when pressed. A key counts as present if either says so — and the report
    calls out the ones where only one of them does, because a key that is documented
    and inert, or live and undocumented, is worth a look either way.
    """
    qt_keys = {str(entry["key"]): entry for entry in qt.get("keys", [])}
    documented = expand_keys(web.get("help", {}).get("Key", []))
    probed = web.get("keys", {})

    matched: List[str] = []
    missing: List[str] = []
    doc_only: List[str] = []
    live_only: List[str] = []
    for token in sorted(qt_keys):
        target = aliases.get(token, token)
        is_documented = target in documented
        evidence = probed.get(target, "unknown")
        if is_documented or evidence == "changed":
            matched.append(token)
            if is_documented and evidence == "inert":
                doc_only.append(token)
            if not is_documented and evidence == "changed":
                live_only.append(token)
        else:
            missing.append(token)

    extra = sorted(set(documented) - {aliases.get(k, k) for k in qt_keys})
    return {
        "section": "keys",
        "matched": [(token, token) for token in matched],
        "moved": [],
        "missing": missing,
        "extra": extra,
        "qt_total": len(qt_keys),
        "web_total": len(documented),
        "qt_index": {},
        "web_index": {},
        "documented_but_inert": doc_only,
        "live_but_undocumented": live_only,
    }


def _index(entries: Iterable[Dict[str, Any]], key: str) -> Dict[str, Dict[str, Any]]:
    out: Dict[str, Dict[str, Any]] = {}
    for entry in entries:
        out.setdefault(normalize_path(str(entry[key])), entry)
    return out


def _leaf(path: str) -> str:
    return path.rsplit("/", 1)[-1]


def diff_section(
    name: str,
    qt_entries: List[Dict[str, Any]],
    web_entries: List[Dict[str, Any]],
    key: str,
    aliases: gaps.AliasMap,
    match_leaf: bool = False,
) -> Dict[str, Any]:
    """Match Qt entries to web entries and split them three ways.

    ``match_leaf`` lets an item match on its own name when its menu path differs —
    the web moved several commands between menus (Catalog Search lives under Data, the
    snufflings are not behind a 'Run Snuffling' submenu), and where a command sits is
    a layout decision, not a missing feature. Where it lands is still reported.
    """
    qt_index = _index(qt_entries, key)
    web_index = _index(web_entries, key)
    web_by_leaf: Dict[str, str] = {}
    for path in web_index:
        web_by_leaf.setdefault(_leaf(path), path)

    matched: List[Tuple[str, str]] = []
    moved: List[Tuple[str, str]] = []
    missing: List[str] = []
    for qt_path in qt_index:
        target = aliases.get(qt_path, qt_path)
        if target in web_index:
            matched.append((qt_path, target))
            continue
        leaf_hit = web_by_leaf.get(_leaf(target)) if match_leaf else None
        if leaf_hit:
            moved.append((qt_path, leaf_hit))
            continue
        missing.append(qt_path)

    claimed = {web for _qt, web in matched} | {web for _qt, web in moved}
    extra = [path for path in web_index if path not in claimed]
    return {
        "section": name,
        "matched": matched,
        "moved": moved,
        "missing": missing,
        "extra": sorted(extra),
        "qt_total": len(qt_index),
        "web_total": len(web_index),
        "qt_index": qt_index,
        "web_index": web_index,
    }


def state_differences(result: Dict[str, Any]) -> List[str]:
    """Where the two agree on the command but not on how it presents itself.

    Reported, never fatal. The interesting case is a Qt radio group whose web
    counterpart publishes no selected state at all: the command is there, but an
    assistive technology cannot tell which of the options is active.
    """
    out: List[str] = []
    for qt_path, web_path in result["matched"] + result["moved"]:
        qt_entry = result["qt_index"].get(qt_path)
        web_entry = result["web_index"].get(web_path)
        if qt_entry is None or web_entry is None:
            continue
        qt_kind, web_kind = qt_entry.get("kind"), web_entry.get("kind")
        if qt_kind and web_kind and qt_kind != web_kind:
            out.append(f"{qt_path}: Qt exposes {qt_kind!r}, the web exposes {web_kind!r}")
    return out


def report(
    result: Dict[str, Any], features: gaps.Allowlist, verbose: bool
) -> Tuple[List[str], List[str]]:
    unexcused: List[str] = []
    name = result["section"]
    states = state_differences(result)
    print(
        f"\n{name}: {len(result['matched'])} matched, {len(result['moved'])} moved, "
        f"{len(result['missing'])} missing, {len(result['extra'])} web-only "
        f"{DIM}(Qt {result['qt_total']}, web {result['web_total']}){RESET}"
    )
    for qt_path, web_path in result["moved"]:
        print(f"  {DIM}moved    {qt_path} -> {web_path}{RESET}")
    for qt_path in result["missing"]:
        entry = features.excuse(qt_path)
        if entry is None:
            unexcused.append(f"{name}: {qt_path}")
            print(f"  {RED}MISSING  {qt_path}{RESET}")
        else:
            # `todo` entries are excused work-in-progress, not accepted differences;
            # the label keeps the two readable apart in the report.
            label = "todo    " if entry.get("todo") else "gap     "
            print(f"  {YELLOW}{label} {qt_path}{RESET}  {DIM}{entry['title']}{RESET}")
    for path in result["extra"]:
        print(f"  {DIM}web-only {path}{RESET}")
    if states:
        print(
            f"  {DIM}state    {len(states)} commands present but exposing a different role{RESET}"
        )
        if verbose:
            for note in states:
                print(f"    {DIM}{note}{RESET}")
    for token in result.get("documented_but_inert", []):
        print(
            f"  {YELLOW}inert    {token} is in the web help but pressing it changed nothing{RESET}"
        )
    for token in result.get("live_but_undocumented", []):
        print(
            f"  {DIM}undoc    {token} does something in the web app but is not in its help{RESET}"
        )
    return unexcused, states


def run(args: argparse.Namespace) -> int:
    regenerate(want_qt=not args.no_qt, want_web=not args.no_web)
    for path in (QT_JSON, WEB_JSON):
        if not path.exists():
            raise SystemExit(f"missing {path}; run without --no-qt/--no-web first")
    qt = json.loads(QT_JSON.read_text())
    web = json.loads(WEB_JSON.read_text())
    register = gaps.Register.load()
    features, aliases = register.features, register.aliases

    results = [
        diff_section("menus", qt["menus"], web["menus"], "path", aliases, match_leaf=True),
        diff_section("controls", qt["controls"], web["controls"], "name", aliases),
    ]
    results.append(diff_keys(qt, web, aliases))
    for section in ("Mouse", "Command"):
        results.append(
            diff_section(
                f"help/{section.lower()}",
                qt["help"].get(section, []),
                web["help"].get(section, []),
                "key",
                aliases,
            )
        )

    unexcused: List[str] = []
    states: List[str] = []
    for result in results:
        section_unexcused, section_states = report(result, features, args.verbose)
        unexcused.extend(section_unexcused)
        states.extend(section_states)

    stale = features.stale()
    idle_aliases = aliases.unused()

    (ARTIFACTS / "tier2.json").write_text(
        json.dumps(
            [
                {
                    key: value
                    for key, value in result.items()
                    if key not in ("qt_index", "web_index")
                }
                for result in results
            ],
            indent=2,
        )
        + "\n"
    )

    print()
    if states:
        print(
            f"{YELLOW}{len(states)} commands are present on both sides but expose a "
            f"different role to assistive technology{RESET}"
        )
        print(
            f"  {DIM}Qt radio groups against plain web actions: the command works, but "
            f"nothing publishes which option is selected. Re-run with -v to list them.{RESET}"
        )
    if idle_aliases:
        # Informational: tier 3 resolves its menu steps through the same table, so an
        # alias idle here may still be earning its keep there.
        print(f"{DIM}{len(idle_aliases)} aliases were never consulted by this diff:{RESET}")
        for key in idle_aliases:
            print(f"  {DIM}{key}{RESET}")
    if stale:
        print(f"{RED}known_gaps.toml lists features that are no longer missing:{RESET}")
        for path in stale:
            print(f"  {RED}{path}{RESET}")
        print(f"  {DIM}remove them, or the allowlist stops describing reality{RESET}")
    if unexcused:
        print(f"{RED}{len(unexcused)} Qt commands are missing and unexcused:{RESET}")
        for item in unexcused:
            print(f"  {RED}{item}{RESET}")
        return 1
    if stale:
        return 1
    print(f"{GREEN}every Qt command is present, moved, or an accounted-for gap{RESET}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-qt", action="store_true", help="reuse the last Qt inventory")
    parser.add_argument(
        "--no-web",
        action="store_true",
        help="reuse the last web inventory (skips needing the app served)",
    )
    parser.add_argument("-v", "--verbose", action="store_true")
    return run(parser.parse_args())


if __name__ == "__main__":
    sys.exit(main())
