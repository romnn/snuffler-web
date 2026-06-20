from typing import Any

d2r: float
earthradius: float
km: float

class UnknownClassicPhase(Exception):
    pass

class PhaseDefParseError(Exception):
    pass

class PhaseDef:
    name: str
    def __init__(self, definition: str) -> None: ...
    @staticmethod
    def classic(name: str) -> list[PhaseDef]: ...

class GradientLayer:
    pass

def load_model(path: str) -> Any: ...
