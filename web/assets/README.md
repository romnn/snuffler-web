# Assets

Runtime assets used by the Python worker and SeisBench inference.
The build stages the required files individually; this README is repository documentation
and is not deployed.

## SeisBench models

`seisbench_models.json` describes the exported models, their inputs, and picker defaults.
`models/` contains the ONNX weights named by that manifest.
`pybuild/export_seisbench_models.py` exports both from the installed SeisBench models.
The default build stages the weights; lean builds can use an external model host.

## Station regions

`station-regions.json` contains Natural Earth 5.1.1 administrative regions and country
names, with official cantons for Luxembourg.
Shared polygon boundaries are simplified together at 0.005 degrees and coordinates
are delta-encoded at 1e-4 degree precision.
Administrative polygons store land regions without a dense grid.
The app reads it on demand from the same origin and retains station lookup results only in RAM.
Station labels consistently use region and country names.
The axis shows the chosen event’s epicentral region; offshore events use the bundled
Flinn–Engdahl geographic lookup rather than a catalog's free-form event title.
Common long country names are shortened for display, such as USA, UK, and UAE.

Sources:

- [Natural Earth Admin 1](https://www.naturalearthdata.com/downloads/10m-cultural-vectors/10m-admin-1-states-provinces/),
  [public domain](https://www.naturalearthdata.com/about/terms-of-use/).
  Archive SHA-256: `efc59726337323058f9446210adc96673179cd344e053666ee3d28cb58ba2b05`.
- [Luxembourg administrative boundaries](https://data.public.lu/en/datasets/limites-administratives-du-grand-duche-de-luxembourg/),
  CC0 1.0; the generator uses the canton collection from the pinned 2026-11-05 download.
  File SHA-256: `497e99914a9c3286254536ac02a9219dfbadbf2a5aa8719e76d48a6305813ccc`.
- Flinn–Engdahl geographic regions, 1995 revision, from the
  [savage13 mirror](https://github.com/savage13/flinn_engdahl/tree/f4b5b7f5aaa2235da86590b696f24713173dae5e).
  Its original source is the U.S. Geological Survey's `feregion/fe_1995` FTP archive,
  also cited as a USGS program package in this
  [European Commission report](https://publications.jrc.ec.europa.eu/repository/bitstream/JRC68247/rapid-n_manual_online_last.pdf).
  [USGS policy](https://www.usgs.gov/faqs/are-usgs-reportspublications-copyrighted)
  treats USGS-produced data as public domain in the United States, while noting that
  third-party material may have separate rights.
  The mirror's README says the original regionalization carried no license; its repository
  is BSD-2-Clause, whose complete notice is included in the generated asset.
  Archive SHA-256: `9d1f7b36e1be651256640ed3186a82d992ddf3bd515f337d1ee4cafed90603fc`.

The generator checks all source hashes before replacing the JSON.
The epicentral table assigns a named seismic region globally at one-degree resolution;
its numbered names and latitude-tier boundaries retain the source's representation.

Regenerate with:

```sh
uv run scripts/build-station-regions.py
```

For offline regeneration, pass `--source`, `--luxembourg-source`, and `--fe-source` with the
downloaded files.

JSON stays readable in Git; HTTP compression is handled by the hosting deployment.
This is regional context, not a precise administrative boundary survey.
Station positions outside the land polygons are not assigned a nearby region.
