from typing import Any, Sequence

class Station:
    network: str
    station: str
    location: str
    lat: float
    lon: float
    elevation: float
    depth: float
    def __init__(
        self,
        network: str = ...,
        station: str = ...,
        location: str = ...,
        lat: float = ...,
        lon: float = ...,
        elevation: float = ...,
        depth: float = ...,
    ) -> None: ...

class Event:
    time: float
    lat: float
    lon: float
    depth: float | None
    magnitude: float | None
    name: str
    moment_tensor: Any
    def __init__(
        self,
        time: float = ...,
        lat: float = ...,
        lon: float = ...,
        depth: float = ...,
        magnitude: float | None = ...,
        name: str = ...,
        moment_tensor: Any = ...,
    ) -> None: ...

def load_stations(filename: str) -> Sequence[Station]: ...
def load_events(filename: str) -> Sequence[Event]: ...
