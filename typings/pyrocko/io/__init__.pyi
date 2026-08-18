from collections.abc import Iterable

from pyrocko.trace import Trace

def load(path: str, format: str = ...) -> Iterable[Trace]: ...
def save(
    traces: Iterable[Trace],
    filename_template: str,
    format: str = ...,
) -> list[str]: ...
