from collections.abc import Sequence
from typing import Any

import numpy as np

class FrequencyResponse:
    def evaluate(self, freqs: np.ndarray[Any, Any]) -> np.ndarray[Any, Any]: ...

class ButterworthResponse(FrequencyResponse):
    def __init__(self, *, order: int = ..., corner: float = ..., type: str = ...) -> None: ...

class MultiplyResponse(FrequencyResponse):
    def __init__(self, responses: Sequence[FrequencyResponse] | None = ...) -> None: ...
