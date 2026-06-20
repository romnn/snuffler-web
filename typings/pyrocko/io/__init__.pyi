from collections.abc import Iterable

from pyrocko.trace import Trace

def load(path: str, format: str = ...) -> Iterable[Trace]: ...
