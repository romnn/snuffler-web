"""Write a small synthetic MiniSEED dataset for the demo (noise + a travelling
event packet across 6 stations x 3 components) — mirrors the egui frontend's
synthetic example so both frontends show comparable data."""

import sys

import numpy as np
from pyrocko import io, trace

out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/demo.mseed"

deltat = 0.02  # 50 Hz
n = 30_000  # 600 s
tmin = 1_700_000_000.0
stations = ["STA01", "STA02", "STA03", "STA04", "STA05", "STA06"]
channels = ["HHZ", "HHN", "HHE"]
t = np.arange(n) * deltat
rng = np.random.default_rng(42)

traces = []
for si, sta in enumerate(stations):
    arrival = 180.0 + si * 18.0
    env = np.exp(-((t - arrival) ** 2) / (2.0 * 8.0**2))
    for cha in channels:
        phase = rng.uniform(0.0, 2.0 * np.pi)
        carrier = np.sin(2.0 * np.pi * 3.2 * (t - arrival) + phase) + 0.45 * np.sin(
            2.0 * np.pi * 6.7 * (t - arrival) + 0.3 * phase
        )
        coda = np.exp(-np.maximum(t - arrival, 0.0) / 26.0)
        coda[t < arrival] = 0.0
        burst_noise = rng.standard_normal(n)
        event = env * carrier + 0.22 * coda * burst_noise
        noise = rng.standard_normal(n)
        y = 0.22 * noise + event
        traces.append(
            trace.Trace(
                network="XX",
                station=sta,
                location="",
                channel=cha,
                deltat=deltat,
                tmin=tmin,
                ydata=y.astype(np.float64),
            )
        )

io.save(traces, out)
print(f"wrote {len(traces)} traces -> {out}")
