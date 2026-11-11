"""The pyrocko side of the bridge, running inside the Pyodide worker.

The worker calls `service.call` for every rpc; `service` hands each one to the
module that owns it.
`state` holds the session: the loaded traces, stations, events and caches.
`view` builds the waveform view, `figures` renders the matplotlib figures, and
the larger built-in snufflings each have a module of their own.
"""
