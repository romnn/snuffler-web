from collections.abc import Callable, Iterable, Sequence
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
    full_id: tuple[str, str, str, str, float]
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
    def overlaps(self, tmin: float, tmax: float) -> bool: ...
    def copy(self) -> Trace: ...
    def add(
        self,
        other: Trace,
        interpolate: bool = ...,
        left: float | None = ...,
        right: float | None = ...,
    ) -> None: ...
    def set_ydata(self, ydata: np.ndarray[Any, Any]) -> None: ...
    def get_ydata(self) -> np.ndarray[Any, Any]: ...
    def spectrum(
        self,
        pad_to_pow2: bool = ...,
        tfade: float | None = ...,
        ntrans_min: int | None = ...,
    ) -> tuple[np.ndarray[Any, Any], np.ndarray[Any, Any]]: ...
    def chop(
        self,
        tmin: float,
        tmax: float,
        inplace: bool = ...,
        include_last: bool = ...,
    ) -> Trace: ...
    def transfer(
        self,
        tfade: float = ...,
        freqlimits: tuple[float, float, float, float] | None = ...,
        transfer_function: Any = ...,
        cut_off_fading: bool = ...,
        demean: bool = ...,
        invert: bool = ...,
    ) -> Trace: ...
    def lowpass(
        self,
        order: int,
        corner: float,
        nyquist_warn: bool = ...,
        nyquist_exception: bool = ...,
        demean: bool = ...,
    ) -> None: ...
    def highpass(
        self,
        order: int,
        corner: float,
        nyquist_warn: bool = ...,
        nyquist_exception: bool = ...,
        demean: bool = ...,
    ) -> None: ...
    def bandpass(self, order: int, low: float, high: float) -> None: ...
    def resample(self, deltat: float) -> None: ...
    def downsample(self, ndecimate: int, snap: bool = ..., demean: bool = ...) -> None: ...
    def downsample_to(self, deltat: float) -> None: ...
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

class Taper: ...

class CosTaper(Taper):
    def __init__(self, a: float, b: float, c: float, d: float) -> None: ...
    def __call__(self, y: np.ndarray[Any, Any], x0: float, dx: float) -> None: ...

def nextpow2(i: float) -> int: ...
def degapper(
    traces: Sequence[Trace],
    maxgap: float = ...,
    fillmethod: str = ...,
    deoverlap: str = ...,
    maxlap: float | None = ...,
) -> list[Trace]: ...
def minmax(
    traces: Iterable[Trace],
    key: Callable[[Trace], Any] | None = ...,
    mode: str | float = ...,
    outer_mode: str = ...,
) -> dict[Any, tuple[float, float]]: ...
def rotate(
    traces: Sequence[Trace],
    azimuth: float,
    in_channels: Sequence[str],
    out_channels: Sequence[str],
) -> list[Trace]: ...
