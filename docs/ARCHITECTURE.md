# Architecture

snuffler-web is a static website.
The application runs in the browser: a user interface written in Rust on the main thread, and
pyrocko in Python in a web worker.
The two exchange protobuf messages.
An optional server, snuffler-web-proxy, is needed for live data, because live streams use TCP,
which a browser cannot open.

<p align="center">
  <br>
  <img width="90%" alt="Diagram of the three places code runs. In the browser, the main thread runs the Rust frontend in WebAssembly and TypeScript glue in the JavaScript engine; a Pyodide worker runs a TypeScript relay and the Python bridge on CPython in WebAssembly; an ONNX worker runs the SeisBench models. An optional backend server runs snuffler-web-proxy. FDSN requests go to the external data centers directly, or through the proxy when the browser cannot reach them; live SeedLink streams go through the proxy, and map tiles come straight from the tile servers. Files the user opens go through the main thread, and a Green's-function store folder is mounted into the Pyodide worker." src="architecture.svg">
  <br>
  The browser threads, their runtimes, and the optional proxy.
  <br><br>
</p>

## Division of work

All scientific computation is done by pyrocko, numpy and scipy.
They run unmodified in [Pyodide](https://pyodide.org), a build of CPython for WebAssembly; this
project uses its own Pyodide build with support for threads, OpenMP and SIMD.
The Python package `snuffler_bridge` exposes what the user interface needs as a list of
operations: reading files, computing a view, running a snuffling, drawing a figure.

The Rust code does not compute anything scientific.
To show traces, it requests a view: a time window, the filter settings and a width in pixels.
The worker returns the traces filtered and reduced to that width, as `float32` arrays, and Rust
draws them.

Files the user opens are read by the TypeScript code on the main thread and sent to the worker
as bytes.
A folder with a Green's-function store is mounted into Python's file system in the worker
instead, so pyrocko reads it as it would on disk.
Exports are saved as browser downloads.

The main thread draws every frame, so slow work must not run there.
Python work runs in the worker.
Slow Rust work, such as exporting a large PNG, runs on a separate thread pool.

## Rust crates

The user interface is split so that more than one frontend can be built on the same code
([ADR 5](../adr/0005-toolkit-free-core-for-any-frontend.md)):

- `snuffler-web-core` holds the session state and settings, markers, the `:` command line, the
  client for the Python worker, and the activity ledger described below.
- `snuffler-web-render` draws waveforms, axes and markers through a `DrawTarget` trait, and can
  write SVG.
- `snuffler-web-egui` is the current frontend, built with egui: menus, dialogs, forms and input
  handling.

The first two crates do not depend on any UI toolkit; `task lint:boundaries` checks this.
Another frontend, for example one built with gpui, would be a new crate using the same two.

## The interface between Rust and Python

The protobuf schema in `proto/` defines the interface.
Its `BridgeService` lists every operation with its request and response message.

A call is sent as bytes: the Rust client passes it to TypeScript code on the main thread, which
posts it to the worker.
The worker passes it to a generated Python dispatcher, which calls the operation and sends the
response back the same way.

Each response contains either the result or a `BridgeError` with the Python exception and its
traceback.
Python code never raises an exception into JavaScript, because on the threaded build converting
the traceback to a JavaScript string fails.
On the Rust side, the response becomes a `Result`.

All Python work goes through the `ActivityLedger` in `snuffler-web-core`
([ADR 6](../adr/0006-one-ledger-owns-python-work.md)).
Each operation states in the schema how it is scheduled: which queue it uses, what happens to a
new request while one is pending, and whether it can be cancelled.
Views have their own queue, so the display keeps updating while a long job runs.
To cancel a running job, the ledger sets Pyodide's interrupt flag, which raises
`KeyboardInterrupt` in the running Python code.

The SeisBench pickers run their models with onnxruntime-web, using WebGPU where the browser
supports it, in a separate worker that the Pyodide worker starts
([ADR 3](../adr/0003-seisbench-inference-on-onnxruntime-web.md)).

## Generated code

Code for all three languages is generated from the schema, so a change to a message causes
build errors at every place that uses it:

- Rust: the build script of `snuffler-web-proto` generates the messages with prost, a client
  with one method per operation, and the scheduling information for the activity ledger.
  The build fails if an operation has no scheduling information.
- Python: `buf generate` writes the message classes, type stubs for mypy, and a typed
  dispatcher into `web/proto`.
  mypy then checks that `snuffler_bridge` implements every operation.
- TypeScript: the same `buf generate` run writes message classes for the TypeScript code.

The Trunk build runs `buf generate` first, and so do the Python and TypeScript check tasks.

The schema is only used inside this project.
All three generated parts are built and deployed together, and protobuf messages are never
stored.
Fields can therefore be renamed, renumbered or removed, as long as all users of the field are
updated in the same commit.

## snuffler-web-proxy

The app gets seismic data in two ways, and a browser limits each differently.

**Live data.**
The app can stream waveforms as a station records them, from SeedLink servers such as
EarthScope's `rtserve.earthscope.org` and GEOFON's `geofon.gfz.de`.
SeedLink is a protocol on top of TCP, and a web page cannot open a TCP connection; it can only
make HTTP requests and open WebSockets.
So live streaming needs a server in between: the page opens a WebSocket to the proxy's `/tcp`
endpoint, the proxy opens the TCP connection to the SeedLink server, and it passes the bytes in
both directions for as long as the stream runs.
Without the proxy, the app cannot stream live data from any server.

**Downloads.**
Recorded waveforms, station metadata and earthquake catalogs come from FDSN data centers over
ordinary HTTP.
The browser fetches them directly, as long as the data center allows other websites to read its
responses with CORS headers.
All data centers the app knows do, except eida.bgr.de, which sends the header twice and is
therefore rejected by browsers.
When the browser refuses a request, the app sends it again through the proxy's `/cors`
endpoint, which forwards it and adds the headers; this is the fallback in the diagram.
The same applies to any other data center without CORS headers that a user enters.

The proxy only connects to an allowlist: the FDSN query endpoints of the data centers the app
knows, and its SeedLink servers.
A deployment can add more with `SNUFFLER_PROXY_ALLOW_HTTP_TARGETS` and
`SNUFFLER_PROXY_ALLOW_TCP_TARGETS`.
`/cors` only forwards `GET` and `HEAD` requests, without the browser's cookies and credentials,
and follows a redirect only to a target it allows too.
It is not an open proxy.

The proxy is a small Rust server that runs separately from the site, from its own Docker image
or Helm chart.
A deployment names it in the site's `config.js`, which the page and the worker read when they
start.
Without one, the site still works for files and downloads from data centers that send CORS
headers, but has no live data.

The app reaches a few other services directly, without the proxy:

- Map tiles for the station map, from tile servers such as OpenStreetMap.
- A Green's-function store URL entered in the seismosizer.
- The SeisBench model host of a build that does not include the models.
- The web analytics script a deployment's `config.js` names, if any (`web/analytics.js`).

## Cross-origin isolation

WebAssembly threads need `SharedArrayBuffer`, which browsers only allow on cross-origin isolated
pages.
The development server and the Docker image send the required headers.
GitHub Pages cannot set headers, so there a service worker adds them.

## Comparison with snuffler

The original Qt snuffler is not part of the site, because Qt for Python cannot be built for
WebAssembly.
It runs natively instead, and the parity tests compare the web app against it
([ADR 2](../adr/0002-upstream-qt-is-a-reference.md)), using the recorded datasets in
`examples/`.
