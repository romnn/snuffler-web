from typing import Any

import numpy as np

def spectrogram(
    x: np.ndarray[Any, Any],
    fs: float,
    nperseg: int,
    noverlap: int,
    scaling: str,
) -> tuple[np.ndarray[Any, Any], np.ndarray[Any, Any], np.ndarray[Any, Any]]: ...
