earthradius_equator: float
d2r: float
r2d: float

def cosdelta(alat: float, alon: float, blat: float, blon: float) -> float: ...
def azimuth(alat: float, alon: float, blat: float, blon: float) -> float: ...
def distance_accurate50m(
    alat: float,
    alon: float,
    blat: float,
    blon: float,
    *,
    implementation: str = ...,
) -> float: ...
