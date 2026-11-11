"""Progress, cancellation and JavaScript waits of the rpc being served.

`service.call` installs the worker's callbacks in the context variables here for
the duration of one rpc; long-running code reports through them.
"""

from __future__ import annotations

import asyncio
from contextvars import ContextVar
from typing import Any, Callable, Optional

# The worker's progress callback: (done, total or -1, phase) -> keep going.
# A False return is a cancellation request.
OperationReporter = Callable[[int, int, str], bool]
# The worker's one-shot commit gate: True when the rpc may commit, False when
# cancellation won the race.
CommitGate = Callable[[], bool]
# Brackets an await on JavaScript, so the worker can keep SIGINT out of it.
ExternalWaitCallback = Callable[[], None]

# The callbacks of the rpc being served, set by `service.call` for its duration.
OPERATION_REPORTER: ContextVar[Optional[OperationReporter]] = ContextVar(
    "operation_reporter", default=None
)
BEGIN_EXTERNAL_WAIT: ContextVar[Optional[ExternalWaitCallback]] = ContextVar(
    "begin_external_wait", default=None
)
END_EXTERNAL_WAIT: ContextVar[Optional[ExternalWaitCallback]] = ContextVar(
    "end_external_wait", default=None
)


def report_progress(done: int, total: Optional[int], phase: str) -> None:
    """Report progress to the worker; a ``total`` of None means unknown.

    Raises KeyboardInterrupt when the worker answers with a cancellation.
    """
    reporter = OPERATION_REPORTER.get()
    if reporter is not None and not reporter(done, -1 if total is None else total, phase):
        raise KeyboardInterrupt


async def await_external(awaitable: Any) -> Any:
    """Await JavaScript while keeping SIGINT out of Pyodide's Future resolver."""
    begin_wait = BEGIN_EXTERNAL_WAIT.get()
    end_wait = END_EXTERNAL_WAIT.get()
    if begin_wait is not None:
        begin_wait()
    try:
        return await awaitable
    finally:
        if end_wait is not None:
            end_wait()


async def report_progress_and_yield(done: int, total: Optional[int], phase: str) -> None:
    """Report progress, then yield to the browser's macrotask queue.

    A long loop calls this between steps so a cancellation can land and the
    worker can answer other messages.
    Outside the browser (no ``js`` module) it yields to the asyncio loop instead.
    """
    report_progress(done, total, phase)
    try:
        import js

        await await_external(js.__snufflerYieldToMacrotask())
    except ImportError:
        await await_external(asyncio.sleep(0))
