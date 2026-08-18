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
    name: str | None
    region: str | None
    catalog: str | None
    moment_tensor: Any
    def __init__(
        self,
        time: float = ...,
        lat: float = ...,
        lon: float = ...,
        depth: float = ...,
        magnitude: float | None = ...,
        name: str | None = ...,
        region: str | None = ...,
        catalog: str | None = ...,
        moment_tensor: Any = ...,
    ) -> None: ...
    def get_hash(self) -> str: ...

def load_stations(filename: str) -> Sequence[Station]: ...
def dump_stations(stations: Sequence[Station], filename: str) -> None: ...
def load_events(filename: str) -> Sequence[Event]: ...

# Re-exported from pyrocko.model.station. Reads a bound that may be left open:
# None and 'nan' both mean "no value", anything else must parse as a float.
def float_or_none(s: str | float | None) -> float | None: ...
