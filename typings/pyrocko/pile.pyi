from collections.abc import Callable, Iterator, Sequence
from typing import Any

from pyrocko.trace import Trace

class TracesFile:
    pass

class MemTracesFile(TracesFile):
    def __init__(self, parent: Any, traces: Sequence[Trace]) -> None: ...

class Pile:
    deltatmin: float | None
    deltatmax: float | None
    tmin: float | None
    tmax: float | None
    def __init__(self) -> None: ...
    def is_empty(self) -> bool: ...
    def add_file(self, file: TracesFile) -> None: ...
    def gather_keys(
        self,
        gather: Callable[[Trace], Any],
        selector: Callable[[Trace], bool] | None = ...,
    ) -> list[Any]: ...
    def chopper(
        self,
        tmin: float | None = ...,
        tmax: float | None = ...,
        tinc: float | None = ...,
        tpad: float = ...,
        group_selector: Callable[[Any], bool] | None = ...,
        trace_selector: Callable[[Trace], bool] | None = ...,
        want_incomplete: bool = ...,
        degap: bool = ...,
        maxgap: float = ...,
        maxlap: float | None = ...,
        keep_current_files_open: bool = ...,
        accessor_id: Any = ...,
        snap: tuple[Callable[[float], float], Callable[[float], float]] = ...,
        include_last: bool = ...,
        load_data: bool = ...,
        style: str | None = ...,
    ) -> Iterator[list[Trace]]: ...
