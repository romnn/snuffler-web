from collections.abc import Sequence
from typing import Any

import numpy as np

class NoData(Exception):
    pass

class Trace:
    network: str
    station: str
    location: str
    channel: str
    deltat: float
    tmin: float
    tmax: float
    ydata: np.ndarray[Any, Any]
    nslc_id: tuple[str, str, str, str]
    meta: Any

    def __init__(
        self,
        *,
        network: str = ...,
        station: str = ...,
        location: str = ...,
        channel: str = ...,
        deltat: float = ...,
        tmin: float = ...,
        ydata: np.ndarray[Any, Any] | None = ...,
        meta: Any = ...,
    ) -> None: ...
    def data_len(self) -> int: ...
    def copy(self) -> Trace: ...
    def set_ydata(self, ydata: np.ndarray[Any, Any]) -> None: ...
    def chop(
        self,
        tmin: float,
        tmax: float,
        inplace: bool = ...,
        include_last: bool = ...,
    ) -> Trace: ...
    def transfer(
        self,
        tfade: float,
        freqlimits: tuple[float, float, float, float],
        transfer_function: Any,
        invert: bool = ...,
    ) -> Trace: ...
    def lowpass(
        self,
        order: int,
        frequency: float,
        nyquist_exception: bool = ...,
    ) -> None: ...
    def highpass(
        self,
        order: int,
        frequency: float,
        nyquist_exception: bool = ...,
    ) -> None: ...
    def bandpass(self, order: int, low: float, high: float) -> None: ...
    def sta_lta_right(
        self,
        tshort: float,
        tlong: float,
        scalingmethod: int = ...,
    ) -> None: ...
    def sta_lta_centered(
        self,
        tshort: float,
        tlong: float,
        scalingmethod: int = ...,
    ) -> None: ...
    def peaks(
        self,
        threshold: float,
        tsearch: float,
    ) -> tuple[list[float], list[float]]: ...
    def extend(
        self,
        tmin: float,
        tmax: float,
        fillmethod: str = ...,
    ) -> None: ...
    def append(self, value: np.ndarray[Any, Any]) -> None: ...

def degapper(traces: Sequence[Trace]) -> list[Trace]: ...
