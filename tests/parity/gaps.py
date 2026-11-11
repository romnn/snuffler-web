"""``known_gaps.toml``, loaded, validated, and enforced.

The register is the one file every tier answers to, and this module is the one reader it
has.
It loads all four entry kinds — ``[[numeric]]`` and ``[[defect]]`` for the tier-1/3
divergence excuses, ``[[feature]]`` for the tier-2 allowlist, ``[[alias]]`` for the
vocabulary map — and owns the two judgments the runners share: :func:`evaluate`, which
holds a scenario's findings against the entries it named, and :class:`Allowlist`, which
excuses missing commands and fails stale excuses.

The rule both judgments enforce is the same: an entry has to keep applying.
A divergence nobody wrote down fails the run, and so does a documented divergence that
stopped happening — that second half is what keeps the register describing the present
instead of rotting into folklore.
"""

from __future__ import annotations

import fnmatch
import re
import tomllib
import unicodedata
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

import compare

KNOWN_GAPS = Path(__file__).resolve().parent / "known_gaps.toml"

# The residual models an entry may declare (see `evaluate`).
# "constant": the excused sample divergence must be a per-segment constant offset.
RESIDUAL_MODELS = ("constant",)


def normalize(text: str) -> str:
    """Reduce a label to the command it names.

    The two frontends were written years apart by different people, so they differ in
    ellipsis character, trailing colon, capitalisation and Qt's '&' accelerators without
    differing in what the command *is*.
    Everything past that — 'Open waveform files...' against 'Open files…' — is a real
    wording choice, and belongs in the alias table where a human decided it, not in a
    regex.
    """
    text = unicodedata.normalize("NFKC", text)
    text = text.replace("…", "...").replace("&", "")
    # `__snufflings__()` appends this to every built-in's name at load time; it says
    # where the snuffling came from, not what it does.
    text = re.sub(r"\s*\(builtin\)$", "", text.strip())
    text = re.sub(r"\.\.\.$", "", text.strip())
    text = re.sub(r":$", "", text.strip())
    text = re.sub(r"\s+", " ", text)
    return text.strip().lower()


def normalize_path(path: str) -> str:
    return "/".join(normalize(part) for part in path.split("/"))


class AliasMap(Dict[str, str]):
    """The ``[[alias]]`` table, remembering which entries a diff actually consulted.

    An alias that no diff ever applies is dead weight that silently stops describing
    anything, so :mod:`run_inventory` reports the unused remainder after its run.
    Only hits are recorded — a miss falling back to the key itself is the normal case,
    not a use.
    """

    def __init__(self, entries: Dict[str, str]) -> None:
        super().__init__(entries)
        self.used: set[str] = set()

    def get(self, key: str, default: Optional[str] = None) -> Optional[str]:  # type: ignore[override]
        if key in self:
            self.used.add(key)
            return self[key]
        return default

    def unused(self) -> List[str]:
        return sorted(set(self) - self.used)


class Allowlist:
    """The ``[[feature]]`` half of the register: excuses, and how to spend them.

    An entry names either one exact Qt path or, with ``qt_prefix``, every path that
    starts with it — a whole submenu, or a family of sibling commands that differ only
    by a suffix.
    The per-snuffling help pages are twenty near-identical entries sharing one reason,
    and writing them out twenty times would obscure rather than document.
    Either way an entry has to be *used*: one that stops matching anything is stale and
    fails the run, which is what keeps the list describing the present.
    """

    def __init__(self, entries: List[Dict[str, Any]]) -> None:
        self.exact: Dict[str, Dict[str, Any]] = {}
        self.prefixes: List[Tuple[str, Dict[str, Any]]] = []
        for entry in entries:
            if "qt_prefix" in entry:
                self.prefixes.append((normalize_path(entry["qt_prefix"]), entry))
            else:
                self.exact[normalize_path(entry["qt"])] = entry
        self.used: set[str] = set()

    def excuse(self, path: str) -> Optional[Dict[str, Any]]:
        entry = self.exact.get(path)
        if entry is not None:
            self.used.add(path)
            return entry
        for prefix, prefix_entry in self.prefixes:
            if path.startswith(prefix):
                self.used.add(prefix)
                return prefix_entry
        return None

    def stale(self) -> List[str]:
        declared = set(self.exact) | {prefix for prefix, _entry in self.prefixes}
        return sorted(declared - self.used)


class Register:
    """Everything ``known_gaps.toml`` declares, parsed once and validated.

    An entry anywhere in the register is either signed off — the ``why`` carries a
    justification for the difference — or marked ``todo = true``: open work the diff
    excuses without mistaking it for a decision.
    ``manual`` entries describe differences no tier can observe yet; nothing enforces
    them, which is why they live in their own kind instead of rotting inside an enforced
    one.
    """

    def __init__(
        self,
        entries: Dict[str, Dict[str, Any]],
        features: Allowlist,
        aliases: AliasMap,
        manual: List[Dict[str, Any]],
    ) -> None:
        # [[numeric]] and [[defect]] keyed together: a scenario names an entry,
        # not a section.
        self.entries = entries
        self.features = features
        self.aliases = aliases
        self.manual = manual

    @classmethod
    def load(cls, path: Path = KNOWN_GAPS) -> "Register":
        raw = tomllib.loads(path.read_text())
        entries: Dict[str, Dict[str, Any]] = {}
        for section in ("numeric", "defect"):
            for entry in raw.get(section, []):
                key = entry["key"]
                unknown = set(entry.get("checks", [])) - set(compare.CHECK_KINDS)
                if unknown:
                    raise SystemExit(
                        f"{path.name}: [{key}] names check kinds that do not exist: "
                        f"{sorted(unknown)}"
                    )
                if key in entries:
                    raise SystemExit(f"{path.name}: duplicate key {key!r}")
                residual = entry.get("residual")
                if residual is not None and residual not in RESIDUAL_MODELS:
                    raise SystemExit(
                        f"{path.name}: [{key}] declares unknown residual model {residual!r}"
                    )
                if residual is not None and "samples" not in entry.get("checks", []):
                    raise SystemExit(
                        f"{path.name}: [{key}] declares a residual model but does not "
                        "cover the 'samples' check it would apply to"
                    )
                subjects = entry.get("subjects")
                if subjects is not None:
                    if not isinstance(subjects, dict) or not subjects:
                        raise SystemExit(
                            f"{path.name}: [{key}] subjects must map check kinds to "
                            "lists of glob patterns"
                        )
                    unscoped = set(subjects) - set(entry.get("checks", []))
                    if unscoped:
                        raise SystemExit(
                            f"{path.name}: [{key}] scopes subjects for kinds it does "
                            f"not cover: {sorted(unscoped)}"
                        )
                    for kind, patterns in subjects.items():
                        if (
                            not isinstance(patterns, list)
                            or not patterns
                            or not all(isinstance(pattern, str) for pattern in patterns)
                        ):
                            raise SystemExit(
                                f"{path.name}: [{key}] subjects.{kind} must be a "
                                "non-empty list of glob patterns"
                            )
                entries[key] = {**entry, "section": section}
        manual: List[Dict[str, Any]] = []
        seen_manual: set[str] = set()
        for entry in raw.get("manual", []):
            key = entry.get("key")
            if not key or not entry.get("title") or not entry.get("why"):
                raise SystemExit(f"{path.name}: [[manual]] entries need key, title and why")
            if key in entries or key in seen_manual:
                raise SystemExit(f"{path.name}: duplicate key {key!r}")
            # The kind's whole promise is that a difference nothing can observe is
            # still on a path out of that state. An entry that cannot say what would
            # observe it is a note, and notes rot.
            if not str(entry.get("enforce", "")).strip():
                raise SystemExit(
                    f"{path.name}: [{key}] must say in `enforce` what check would observe it"
                )
            seen_manual.add(key)
            manual.append(entry)
        aliases = AliasMap(
            {
                normalize_path(entry["qt"]): normalize_path(entry["web"])
                for entry in raw.get("alias", [])
            }
        )
        return cls(entries, Allowlist(raw.get("feature", [])), aliases, manual)

    def unreferenced(self, named: Iterable[str]) -> List[str]:
        """Enforced entries that no scenario names.

        :func:`evaluate` only ever looks at the entries a scenario listed in its
        ``expect_divergence``, so an entry nothing names is inert: it excuses no
        divergence and never fails when the divergence stops happening.
        That is indistinguishable from an entry that has quietly rotted, and it is
        exactly what ``[[manual]]`` exists to hold instead.
        Meaningful only over a full run, where ``named`` really is every scenario's
        declaration.
        """
        claimed = set(named)
        return sorted(key for key in self.entries if key not in claimed)


def _covers(entry: Dict[str, Any], finding: compare.Finding) -> bool:
    """Whether one register entry accounts for one finding.

    An entry covers the check kinds it names; ``subjects.<kind>`` narrows a kind
    to the findings whose subject matches one of its glob patterns, so an excuse
    can name the channels a divergence lives on instead of blanketing the kind.
    """
    if finding.kind not in entry.get("checks", []):
        return False
    patterns = entry.get("subjects", {}).get(finding.kind)
    if patterns is None:
        return True
    return any(fnmatch.fnmatchcase(finding.subject, pattern) for pattern in patterns)


def evaluate(
    expect_divergence: List[str],
    findings: Iterable[compare.Finding],
    register: Register,
) -> Tuple[List[str], List[str]]:
    """Hold one scenario's findings against the entries it named.

    Returns ``(problems, defects_hit)``; an empty ``problems`` means the scenario
    behaves exactly as documented.
    Judged finding by finding, in both directions:

    * every failing finding needs a named entry that covers it — its kind, and its
      subject where the entry scopes one — or the divergence is unexplained;
    * every *strict* entry has to keep reproducing, kind by named kind: a kind
      with no covered failing finding left means the entry is stale for it. That
      is what stops the register from decaying into a list of things that used to
      be true. Non-strict entries describe a difference whose size is
      data-dependent — a filter transient that may or may not land on the
      extremum of a particular window — so they explain a divergence without
      asserting one.

    An entry covering ``samples`` may additionally declare ``residual = "constant"``.
    The excuse then asserts a *shape*, not just an existence: every failing sample
    finding the entry covers must vanish once the per-segment constant offset is
    removed, so a second, unrelated sample defect cannot hide behind the entry.
    Scoping and shape both aim at the suite's own recorded failure mode — an entry
    masking every other difference in the checks it claims.
    """
    findings = list(findings)
    problems: List[str] = []
    named: List[Dict[str, Any]] = []
    defects_hit: List[str] = []
    for key in expect_divergence:
        gap = register.entries.get(key)
        if gap is None:
            problems.append(f"expect_divergence names {key!r}, which known_gaps.toml lacks")
            continue
        named.append(gap)
        if gap["section"] == "defect":
            defects_hit.append(key)

    failing = [finding for finding in findings if not finding.ok]

    # No undeclared divergence — aggregated per kind, so a regression sweeping
    # hundreds of tracks reads as one problem with a count, not a wall of lines.
    uncovered: Dict[str, List[compare.Finding]] = {}
    for finding in failing:
        if not any(_covers(gap, finding) for gap in named):
            uncovered.setdefault(finding.kind, []).append(finding)
    for kind in sorted(uncovered):
        group = uncovered[kind]
        first = group[0]
        count = f" ({len(group)} findings)" if len(group) > 1 else ""
        problems.append(
            f"unexplained divergence in {kind}{count} — {first.label()}: {first.detail}"
        )

    for gap in named:
        if not gap.get("strict", True):
            continue
        for kind in gap.get("checks", []):
            if not any(finding.kind == kind and _covers(gap, finding) for finding in failing):
                problems.append(
                    f"{kind} was expected to diverge and did not; the known_gaps.toml "
                    f"entry {gap['key']!r} is stale for it and should be updated or removed"
                )

    # The residual model: an excuse that names a shape has to see that shape, and
    # it applies to every failing finding the declaring entry covers — including
    # one another entry would have excused unconditionally.
    for finding in failing:
        if finding.kind != "samples":
            continue
        models = sorted(
            gap["key"] for gap in named if gap.get("residual") and _covers(gap, finding)
        )
        if not models:
            continue
        # Only "constant" exists today; Register.load rejected anything else.
        centered = finding.metrics.get("max_abs_centered")
        bound = finding.metrics.get("bound")
        if centered is None or bound is None or centered > bound:
            shown = "n/a" if centered is None else f"{centered:.3e}"
            problems.append(
                f"{finding.label()}: the divergence is not the constant offset "
                f"{', '.join(models)} describes (max|Δ-meanΔ|={shown} exceeds the tolerance)"
            )
    return problems, defects_hit
