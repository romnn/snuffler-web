"""The NSLC channel id (network, station, location, channel) and its proto form."""

from __future__ import annotations

from typing import Tuple

from snuffler.bridge.v1 import common_pb2

# pyrocko's ``nslc_id``: (network, station, location, channel) codes.
NslcId = Tuple[str, str, str, str]


def nslc_message(value: NslcId) -> common_pb2.Nslc:
    """The proto form of an nslc id."""
    net, sta, loc, cha = value
    return common_pb2.Nslc(net=net, sta=sta, loc=loc, cha=cha)
