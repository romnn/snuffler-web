# Example datasets

Real recordings, checked in. Each directory here is one dataset, and each one does
two jobs at once:

- **`File ▸ Examples` in the app** loads it. `scripts/build-examples.py` packs the
  directory into a single zip that the deployed site serves from
  `assets/examples/`, and the app fetches it only when someone picks it — so the
  page costs nothing to load and a laptop with no data on it is still a laptop that
  can learn snuffler. Each has a shareable link: `?example=<slug>`.
- **The parity suite** measures against every one of them (`tests/parity/corpus/build.py`
  pins every file by sha256). So the bytes a student downloads are the bytes both
  frontends are proven to agree on, rather than a second copy that drifted — and a
  dataset cannot be shipped here without the two viewers being held to it.

| dataset | what it shows | bundle |
| --- | --- | --- |
| `bonin-2015-05-30-m7.8-multiloc` | six GSN stations each running two or three co-located sensors, 664 km deep source | 1.7 MB |
| `tohoku-2011-03-11-m9.1-clipping` | one site, three instruments: the broadband clips, the accelerometer stays on scale | 0.5 MB |
| `sumatra-2004-12-26-m9.1-surface-waves` | six hours at 1 Hz: dispersion and multi-orbit surface waves (R2, R3) | 0.8 MB |
| `ridgecrest-2019-07-06-m7.1-local` | a local earthquake at 100 Hz, 5 to 89 km away | 0.7 MB |
| `tonga-2022-01-15-air-wave` | the eruption's atmospheric Lamb wave at 306 m/s, on co-sited barometers | 1.4 MB |
| `kilauea-2018-07-15-caldera-collapse` | a caldera collapse event, with its precursory swarm and the quiet after | 5.7 MB |
| `eifel` | a real deployment: 354 stations, six networks, three sampling rates | 20.0 MB |

They are meant to differ from each other. Between them they cover teleseismic and
local, 1 Hz and 100 Hz, one station and 354, body waves and surface waves, ground
motion and air pressure, faults and a volcano, sensors that agree and sensors that
do not — so "load an example" is a real choice rather than a single canned demo.

## What a dataset directory holds

```
<slug>/
  example.toml     what it is, its characteristics, where the bytes came from
  <slug>.mseed     waveforms
  catalog.yaml     pyrocko event catalogue
  meta/*.xml       StationXML, one file per network, response level
```

`example.toml` describes the dataset; everything else *is* the dataset and goes into
the bundle. Three parts of it matter:

- `title`, `blurb`, `characteristics`, `citation` — what the menu shows, and what
  the `About this dataset` window shows once it is loaded. The blurb's first
  paragraph is the menu tooltip. `characteristics` is what is unique about this
  recording, not a list of instructions: the window is headed "What is unique
  about this dataset", and "Sort by Distance" does not answer that where
  "distances from 86° to 136°" does. Naming the control that shows a
  characteristic is welcome; the characteristic comes first. Keep the tone plain:
  state what the data shows and why, in terms a student of geophysics can check,
  and claim nothing the bytes in this directory cannot back.
- `[state]` — the view to open in. An example exists to show something and the
  setting that shows it is part of the example: co-located sensors say nothing
  until `subsort = "nsc_l"` groups them onto one row. Only these bundles carry it;
  a zip you assemble yourself opens in whatever state you were already in.
- `[source]` — the datacenter queries the bytes came from, replayed by
  `scripts/fetch-example.py <slug>` (`--dry-run` prints them without fetching).

That script is provenance, not a generator. StationXML carries a `<Created>` stamp
and datacenters revise metadata, so re-running it does not reproduce the checked-in
bytes; for a dataset the parity suite measures, that drifts
`tests/parity/corpus/manifest.json` and the two have to move in one commit. Re-fetch
to change a dataset on purpose. `eifel` carries no `[source]`: its bytes can only be
restored from git.

## The bundle format

A bundle is a plain zip. Members are dispatched by what they are — `.xml` is
StationXML, guts yaml is read for its `!pf.Event` / `!pf.Station` tag, anything
unrecognized is handed to `io.load(format='detect')` — so **any zip of data files
loads**, not only the ones built here. That is deliberate: handing out one file
beats handing out a folder and instructions, and the app accepts a bundle from the
file picker or dropped on the window exactly as it accepts a waveform file.

The shipped bundles additionally carry a `snuffler.json` (the `example.toml`
fields), which is what the `About this dataset` window shows and where `[state]`
travels. A bundle without it still loads and reports what it loaded; it just has
nothing to say about itself, so the Help entry that reopens that window is not
offered. Nothing about this is limited to the bundles built here — write a
`snuffler.json` into your own zip and it describes itself the same way.

Bundles are built deterministically — fixed member timestamps, fixed permissions,
sorted order — so rebuilding unchanged inputs produces identical bytes and nothing
downstream re-uploads.

## Adding one

1. Write an `example.toml`: `[source]` for the queries, then `title`, `blurb`,
   `characteristics`, `citation`, and a `[state]` if the dataset has a view that
   makes its point.
2. `uv run --project qt python scripts/fetch-example.py <slug>`.
3. **Check what you actually got**, and write the blurb from that rather than from
   what the event ought to look like. Every number in the descriptions here was
   measured off the bytes: the ratio between MAJO's two broadbands collapsing from
   5.4 to 1.4, Sumatra's second orbit at a fifth of the first, Ridgecrest's 6 km/s
   moveout. An example that claims something the data does not show teaches the
   wrong thing twice.
4. `task build:examples` and check the size. Anything past ~20 MB deserves a
   second look — it is one click for a user on a phone.
5. Add it to the parity corpus — every dataset here is in it, so this is not
   optional: its files in `CHECKED_IN` (`tests/parity/corpus/build.py`), a `Dataset`
   in `tests/parity/scenario.py`, at least a control scenario with every option off
   plus one that exercises whatever makes the dataset worth shipping, then
   `python tests/parity/corpus/build.py --update-manifest`. If the window is long or
   the rate high, the scenario needs a `visible_length` that lets Qt keep every trace
   — `tests/parity/README.md` explains why.

## Provenance

These are other people's recordings. `example.toml` carries the citation each
network asks for and the app shows it under `Help ▸ About this dataset`; keep it
accurate, and check that a dataset is openly licensed before adding it — the GSN,
SCSN, HVO and Eifel networks here are.

One thing the fetch is allowed to change: MiniSEED records carrying no samples are
dropped. Datacenters ship them (Sumatra's window came back with nine), pyrocko reads
each as a trace, and a viewer then draws a blank row for something that is not a
channel — while pyrocko's own `pile` refuses the file outright, so snuffler could not
open the dataset at all. Records are deleted whole rather than re-encoded, so every
sample and start time in what remains is still the datacenter's own.
