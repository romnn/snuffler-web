"""Process scaffolding every parity entry point imports first.

Importing this module is what makes the runners work as plain scripts: it
prepares the environment before anything imports pyrocko or the bridge, and it
owns the ANSI palette the runners print with, so "no colour when piped" is
decided once.

The environment half has to run at import time, before the importing module's
next ``import`` statement executes — which is why every entry point imports
``harness`` ahead of anything that pulls in pyrocko, Qt or ``bridge``.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
ARTIFACTS = HERE / ".artifacts"

# pyrocko reads its config directory once, at import time, and `PileViewerMain`
# constructs `config.config('snuffler')` — which would both pick up the developer's
# own snuffler preferences as scenario defaults and write into their home. Point it
# at a scratch directory before anything imports pyrocko.
os.environ.setdefault("PYROCKO_DIR", str(ARTIFACTS / "pyrocko-home"))
os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
# The bridge and the generated protobuf package are not installed, they are read
# out of the working tree, exactly as the Pyodide worker reads them.
for _extra in (ROOT / "web", ROOT / "web" / "proto", HERE):
    if str(_extra) not in sys.path:
        sys.path.insert(0, str(_extra))

GREEN, RED, YELLOW, DIM, RESET = "\033[32m", "\033[31m", "\033[33m", "\033[2m", "\033[0m"
if not sys.stdout.isatty() or os.environ.get("NO_COLOR"):
    GREEN = RED = YELLOW = DIM = RESET = ""
