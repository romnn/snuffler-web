"""Fit pigment palettes jointly on white and near-black; emit plain Rust tables.

Run with `uv run python scripts/fit-wiggle-themes.py` from the repository root.
Candidates have fixed hues, bounded chroma and bounded contrast before fitting.
The objective saturates separation targets and then favours the designed tones.
All measurements use the rounded sRGB bytes that the renderer actually receives.
"""

import re
from pathlib import Path

import numpy as np

# Each row is an ordered family: three lead pigments, then four supporting tones.
# The same order serves both canvases and both carriers of class identity.
DESIGNS = {
    "house": (
        "Fieldbook",
        [255, 25, 145, 305, 85, 195, 355],
        [0.130, 0.130, 0.120, 0.110, 0.100, 0.090, 0.100],
    ),
    "earthy": (
        "Kiln",
        [40, 245, 145, 85, 5, 190, 310],
        [0.130, 0.120, 0.110, 0.100, 0.110, 0.080, 0.090],
    ),
    "cool": (
        "Fjord",
        [250, 55, 155, 295, 205, 355, 105],
        [0.120, 0.110, 0.115, 0.110, 0.095, 0.100, 0.100],
    ),
    "sky": (
        "Daybreak",
        [280, 100, 15, 235, 170, 325, 60],
        [0.130, 0.110, 0.120, 0.110, 0.090, 0.105, 0.090],
    ),
    "forest": (
        "Herbarium",
        [150, 10, 270, 195, 100, 325, 55],
        [0.125, 0.130, 0.120, 0.090, 0.100, 0.110, 0.100],
    ),
    "sunset": (
        "Afterglow",
        [20, 265, 140, 320, 65, 205, 355],
        [0.130, 0.130, 0.100, 0.110, 0.110, 0.080, 0.100],
    ),
}

LMS = np.array(
    [
        [0.4122214708, 0.5363325363, 0.0514459929],
        [0.2119034982, 0.6806995451, 0.1073969566],
        [0.0883024619, 0.2817188376, 0.6299787005],
    ]
)
LAB = np.array(
    [
        [0.2104542553, 0.7936177850, -0.0040720468],
        [1.9779984951, -2.4285922050, 0.4505937099],
        [0.0259040371, 0.7827717662, -0.8086757660],
    ]
)
PAIRS = np.triu_indices(7, 1)

# Apply Machado severity-1 matrices in linear RGB before display-gamut clipping.
# https://www.inf.ufrgs.br/~oliveira/pubs_files/CVD_Simulation/CVD_Simulation.html
MACHADO = np.array(
    [
        [
            [0.152286, 1.052583, -0.204868],
            [0.114503, 0.786281, 0.099216],
            [-0.003882, -0.048116, 1.051998],
        ],
        [
            [0.367322, 0.860646, -0.227968],
            [0.280085, 0.672501, 0.047413],
            [-0.011820, 0.042940, 0.968881],
        ],
        [
            [1.255528, -0.076749, -0.178779],
            [-0.078411, 0.930809, 0.147602],
            [0.004733, 0.691367, 0.303900],
        ],
    ]
)


def linear(rgb):
    value = rgb / 255.0
    return np.where(value <= 0.04045, value / 12.92, ((value + 0.055) / 1.055) ** 2.4)


def lab(rgb):
    return np.cbrt(linear(rgb) @ LMS.T) @ LAB.T


def luminance(rgb):
    return linear(rgb) @ np.array([0.2126, 0.7152, 0.0722])


def appearances(rgb):
    simulated = [
        np.cbrt(np.clip(linear(rgb) @ matrix.T, 0, 1) @ LMS.T) @ LAB.T for matrix in MACHADO
    ]
    return np.stack([lab(rgb), *simulated], axis=-2).reshape(-1, 12)


def candidates(hue, chroma, dark, strokes, house):
    if strokes:
        levels = np.linspace(0.70, 0.85, 41) if dark else np.linspace(0.40, 0.57, 45)
        chromas = np.linspace(max(0.065, chroma - 0.018), chroma + 0.018, 17)
        preferred = 0.77 if dark else 0.49
        minimum = (8.0 if house else 7.0) if dark else (6.9 if house else 6.3)
        maximum = 12.5 if dark else 11.0
    else:
        levels = np.linspace(0.27, 0.43, 41) if dark else np.linspace(0.83, 0.95, 41)
        chroma *= 0.48
        chromas = np.linspace(chroma * 0.75, chroma * 1.25, 17)
        preferred = 0.35 if dark else 0.90
        minimum, maximum = (1.16, 2.2) if dark else (1.12, 1.90)
    level, saturation = np.meshgrid(levels, chromas)
    angle = np.deg2rad(hue)
    values = np.stack(
        [level.ravel(), saturation.ravel() * np.cos(angle), saturation.ravel() * np.sin(angle)],
        axis=-1,
    )
    rgb_linear = ((values @ np.linalg.inv(LAB).T) ** 3) @ np.linalg.inv(LMS).T
    in_gamut = np.all((rgb_linear >= 0) & (rgb_linear <= 1), axis=-1)
    rgb_linear = np.clip(rgb_linear, 0, 1)
    rgb = np.rint(
        255
        * np.where(
            rgb_linear <= 0.0031308, rgb_linear * 12.92, 1.055 * rgb_linear ** (1 / 2.4) - 0.055
        )
    )
    ground = luminance(np.array([18, 19, 24])) if dark else 1.0
    lum = luminance(rgb)
    contrast = (lum + 0.05) / (ground + 0.05) if dark else 1.05 / (lum + 0.05)
    keep = in_gamut & (contrast >= minimum) & (contrast <= maximum)
    rgb = np.unique(rgb[keep], axis=0)
    measured = lab(rgb)
    cost = ((measured[:, 0] - preferred) / 0.10) ** 2 + 3 * (
        (np.linalg.norm(measured[:, 1:], axis=-1) - chroma) / 0.035
    ) ** 2
    return rgb, appearances(rgb), cost


def fit(hues, chromas, strokes, house):
    pools = [
        candidates(h, c, dark, strokes, house)
        for dark in (False, True)
        for h, c in zip(hues, chromas, strict=True)
    ]
    targets = np.full((7, 7), 0.108 if strokes else 0.070)
    if strokes:
        targets[:3, :3] = 0.195
        targets[:2, :2] = 0.235
    targets = targets[PAIRS]

    def score(values, costs):
        shaped = values.reshape(-1, 2, 7, 4, 3)
        all_distances = np.linalg.norm(shaped[:, :, PAIRS[0]] - shaped[:, :, PAIRS[1]], axis=-1)
        distances = all_distances[:, :, :, 0].min(axis=1)
        # The worse canvas owns each pair's score, so neither can spend its margin
        # on a different ordering or leave a collision for the other to absorb.
        deficit = np.maximum(targets - distances, 0)
        # CVD is a soft tie-break within the bounded pigment family, never a reason
        # to sacrifice the normal-vision floors or turn a supporting tone neon.
        cvd = all_distances[:, :, :, 1:].min(axis=(1, 2, 3))
        return (
            1e6 * np.sum(deficit**2, axis=-1) + costs.sum(axis=-1) - 0.2 * np.minimum(cvd * 100, 6)
        )

    rng = np.random.default_rng(20260905)
    best = None
    for restart in range(8):
        indices = [
            int(np.argmin(p[2])) if restart == 0 else int(rng.integers(len(p[0]))) for p in pools
        ]
        values = np.array([p[1][i] for p, i in zip(pools, indices, strict=True)])
        costs = np.array([p[2][i] for p, i in zip(pools, indices, strict=True)])
        for _ in range(24):
            changed = False
            for slot in rng.permutation(14):
                _, options, penalties = pools[slot]
                batch = np.broadcast_to(values, (len(options), 14, 12)).copy()
                batch[:, slot] = options
                batch_costs = np.broadcast_to(costs, (len(options), 14)).copy()
                batch_costs[:, slot] = penalties
                choice = int(np.argmin(score(batch, batch_costs)))
                changed |= choice != indices[slot]
                indices[slot] = choice
                values[slot], costs[slot] = options[choice], penalties[choice]
            if not changed:
                break
        loss = float(score(values, costs)[0])
        if best is None or loss < best[0]:
            best = (
                loss,
                np.array([p[0][i] for p, i in zip(pools, indices, strict=True)]).reshape(2, 7, 3),
            )
    if best is None:
        raise RuntimeError("No palette candidates")
    result = best[1]
    distances = np.linalg.norm(lab(result)[:, PAIRS[0]] - lab(result)[:, PAIRS[1]], axis=-1)
    print(
        "strokes" if strokes else "fills",
        "worst pair",
        np.round(distances.min(axis=1) * 100, 2),
        "first pair",
        np.round(distances[:, 0] * 100, 2),
    )
    return result


def main():
    root = Path(__file__).resolve().parents[1]
    updates = {}
    for module, (name, hues, chromas) in DESIGNS.items():
        print(name, flush=True)
        strokes = fit(hues, chromas, True, module == "house")
        fills = fit(hues, chromas, False, module == "house")
        path = root / "crates/snuffler-web-render/src/themes" / f"{module}.rs"
        source = path.read_text()
        for carrier, palette in (("STROKES", strokes), ("FILLS", fills)):
            for polarity, colors in zip(("LIGHT", "DARK"), palette, strict=True):
                rows = "\n".join(
                    "    (" + ", ".join(f"0x{int(c):02X}" for c in color) + ")," for color in colors
                )
                source, count = re.subn(
                    rf"(const {polarity}_{carrier}: \[Rgb; 7\] = \[).*?\n\];",
                    rf"\g<1>\n{rows}\n];",
                    source,
                    flags=re.S,
                )
                if count != 1:
                    raise ValueError(f"Missing or duplicate {polarity}_{carrier} table in {path}")
        updates[path] = source
    # Fit every family before writing; the generic Rust tests own acceptance floors.
    for path, source in updates.items():
        path.write_text(source)


if __name__ == "__main__":
    main()
