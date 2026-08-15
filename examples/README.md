# Example datasets

Real recordings, checked in. Each directory here is one dataset, and each one does
two jobs at once:

- **`File ▸ Examples` in the app** loads it. `scripts/build-examples.py` packs the
  directory into a single zip that the deployed site serves from
  `assets/examples/`, and the app fetches it only when someone picks it — so the
  page costs nothing to load and a laptop with no data on it is still a laptop that
  can learn snuffler. Each has a shareable link: `?example=<slug>`.
- **The parity suite** measures against some of them (`tests/parity/corpus/build.py`
  pins every file it uses by sha256). Where it does, the bytes a student downloads
  are the bytes both frontends are proven to agree on, rather than a second copy
  that drifted.

| dataset | what it shows | bundle |
| --- | --- | --- |
| `bonin-2015-05-30-m7.8-multiloc` | six GSN stations each running two or three co-located sensors, 664 km deep source | 1.7 MB |
| `tohoku-2011-03-11-m9.1-clipping` | one site, three instruments, and the sensitive one going off scale | 0.5 MB |
| `sumatra-2004-12-26-m9.1-surface-waves` | six hours at 1 Hz: dispersion, and the wave coming back round | 0.8 MB |
| `ridgecrest-2019-07-06-m7.1-local` | a local earthquake at 100 Hz, 5 to 89 km away | 0.7 MB |
| `eifel` | a real deployment: 354 stations, six networks, three sampling rates | 20.0 MB |

They are meant to differ from each other. Between them they cover teleseismic and
local, 1 Hz and 100 Hz, one station and 354, body waves and surface waves, sensors
that agree and sensors that do not — so "load an example" is a real choice rather
than a single canned demo.

## What a dataset directory holds

```
<slug>/
  example.toml     what it is, what it teaches, where the bytes came from
  <slug>.mseed     waveforms
  catalog.yaml     pyrocko event catalogue
  meta/*.xml       StationXML, one file per network, response level
```

`example.toml` describes the dataset; everything else *is* the dataset and goes into
the bundle. Four of its tables matter:

- `title`, `blurb`, `teaches`, `citation` — what the menu shows, and what the load
  report shows afterwards. The blurb's first paragraph is the menu tooltip.
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
to change a dataset on purpose. `eifel` predates the convention and carries no
`[source]`: its bytes can only be restored from git.

## The bundle format

A bundle is a plain zip. Members are dispatched by what they are — `.xml` is
StationXML, guts yaml is read for its `!pf.Event` / `!pf.Station` tag, anything
unrecognized is handed to `io.load(format='detect')` — so **any zip of data files
loads**, not only the ones built here. That is deliberate: handing out one file
beats handing out a folder and instructions, and the app accepts a bundle from the
file picker or dropped on the window exactly as it accepts a waveform file.

The shipped bundles additionally carry a `snuffler.json` (the `example.toml`
fields), which is what the load report shows and where `[state]` travels. A bundle
without it still loads; it just says less.

Bundles are built deterministically — fixed member timestamps, fixed permissions,
sorted order — so rebuilding unchanged inputs produces identical bytes and nothing
downstream re-uploads.

## Adding one

1. Write an `example.toml`: `[source]` for the queries, then `title`, `blurb`,
   `teaches`, `citation`, and a `[state]` if the dataset has a view that makes its
   point.
2. `uv run --no-project --python 3.12 --with numpy==2.4.3 --with ./sources/pyrocko
   python scripts/fetch-example.py <slug>`.
3. **Check what you actually got**, and write the blurb from that rather than from
   what the event ought to look like. Every number in the descriptions here was
   measured off the bytes: the ratio between MAJO's two broadbands collapsing from
   5.4 to 1.4, Sumatra's second orbit at a fifth of the first, Ridgecrest's 6 km/s
   moveout. An example that claims something the data does not show teaches the
   wrong thing twice.
4. `mise run build-examples` and check the size. Anything past ~20 MB deserves a
   second look — it is one click for a user on a phone.
5. Optionally add it to the parity corpus: files in `CHECKED_IN`
   (`tests/parity/corpus/build.py`), a `Dataset` in `tests/parity/scenario.py`, a
   scenario, then `python tests/parity/corpus/build.py --update-manifest`. Worth it
   for a dataset that exercises something the suite should hold; git already
   protects the rest from silent edits.

## Provenance

These are other people's recordings. `example.toml` carries the citation each
network asks for and the app shows it in the load report; keep it accurate, and
check that a dataset is openly licensed before adding it — the GSN, SCSN and Eifel
networks here are.
