from typing import Any, Iterable

class _Value:
    value: float

class Channel:
    pass

class Station:
    code: str
    latitude: _Value
    longitude: _Value
    elevation: _Value
    channel_list: list[Channel]

class Network:
    code: str
    station_list: list[Station]

class StationXml:
    network_list: list[Network]
    def get_pyrocko_response(
        self,
        nslc_id: tuple[str, str, str, str],
        timespan: tuple[float, float],
        fake_input_units: str = ...,
    ) -> Any: ...
    def get_pyrocko_stations(self) -> list[Any]: ...

def load_xml(string: str) -> StationXml: ...
def primitive_merge(sxs: Iterable[StationXml]) -> StationXml: ...
