---
name: qa
description: >-
  How to drive the running snuffler-web app from a browser agent (Playwright MCP,
  WebDriver, claude-in-chrome) to verify a change. Use this BEFORE clicking
  anything in this app — it renders to a canvas, so `?a11y=pointer` and the DOM
  projection are what make it addressable at all.
---

# Driving snuffler-web

The UI is **egui painted into a single `<canvas>`**. There are no real buttons.
What a browser agent sees instead is a projection of egui's AccessKit tree into a
DOM overlay (`accesskit-dom` → `#snuffler-a11y`), built for exactly this purpose:
one invisible element per widget, carrying its role, name, and value.

## Always start here

```
http://127.0.0.1:17683/?a11y=pointer
```

**Without `?a11y=pointer` you cannot click anything by element.** The proxies are
`pointer-events: none` on a normal visit, so a press falls through to the canvas
where egui does its own hit-testing. That default exists because a proxy that
hit-tests is handed the press meant for the canvas and can only report a *click*
back — a press‑move‑release dies there, and dragging a slider, sweeping a
selection, or hovering stops working for the human using the app. The flag turns
hit-testing on for your session only.

If the app is not running: `task run` (or the `web` service in `micromux.yaml`,
port 17683; the FDSN/SeedLink proxy on 17684 is a separate service).

## Waiting for readiness

Pyodide boots a real CPython + pyrocko, which takes tens of seconds:

1. Wait for the text `Importing pyrocko...` to be **hidden** (the boot overlay
   covers everything until then, and it is `pointer-events: none` but opaque).
2. Then wait for the status line to read `Ready · N traces`.

Never assert on the UI before both; a screenshot taken early shows the splash.

## Finding and reading widgets

Everything lives under `#snuffler-a11y`:

| Want | How |
| --- | --- |
| A control | `[role=button]`, `[role=checkbox]`, `[role=slider]`, `[role=combobox]`, `[role=textbox]` |
| By name | `aria-label` (e.g. `[aria-label="Deconvolution"]`) or `getByRole(role, { name })` |
| Its state | `aria-checked`, `aria-valuenow`, `aria-valuemin`/`max`, `aria-expanded` |
| Tabbable controls | `[tabindex="0"]` |
| Status/announcements | the live region (`aria-live`) in the top bar |

Names are often **borrowed from the label beside the widget** (the "Highpass
[Hz]:" text names the field next to it), so search by the label you see painted.

## Interacting

- **Click / activate**: `locator.click()` works with `?a11y=pointer`. Without the
  flag, use `el.click()` via `$eval` — a synthetic click still bubbles to the
  overlay's delegated listener and becomes an `Action::Click`.
- **Keyboard**: `el.focus()` then `Enter`/`Space` (activate), arrows (sliders).
  This path works in every mode and does not depend on hit-testing.
- **Direct manipulation — panning, zooming, dragging a slider, placing a pick —
  must go to the canvas**, never to a proxy: `page.mouse.move/down/up` over
  `#the_canvas_id`. A proxy click cannot express a drag. Read the proxy's
  `getBoundingClientRect()` to find where the widget is, then drive the mouse
  there.
- **Verify a drag worked** by reading `aria-valuenow` back, not by screenshot.

## The `:` command line

`:` opens it (Escape closes), `↑`/`↓` walk the history, Return runs. It mirrors
pyrocko's snuffler: `n|s|l|c <pattern>` to reduce traces, `hide`/`unhide`,
`markers`, `scaling`, `goto`. A rejected command keeps the input open, tints it,
and prints the reason underneath — a fast way to assert on error handling.
Because it is a focused text field, single-key shortcuts are suppressed while it
is open.

## Flags

| Flag | Effect |
| --- | --- |
| `?a11y=pointer` | Proxies become the hit target — **use this to drive the app** |
| `?a11y=debug` | Publishes the tree as JSON on `window.__snufflerA11y` (+ `…Rev` counter) |
| `?a11y=tree` | egui builds the AccessKit tree, nothing is projected (profiling) |
| `?a11y=0` | Layer off entirely |

`window.wasmBindings.snuffler_a11y_stats()` returns projection cost
(`passes`, `totalMs`, `described`, `placed`).

## Running it, and knowing when a change is actually served

The app runs under **micromux** (`micromux.yaml`): the `web` service is the Trunk
dev server on 17683, `proxy` is the FDSN/SeedLink proxy on 17684. Use the
micromux MCP tools — `list_services`, `get_health`, `get_logs`, `wait_for_log`,
`restart_service` — rather than starting your own server on the same port; a
second one collides with the service-worker namespace the COI headers rely on.

**Wait for the right log line.** Trunk builds into `dist/.stage` and only swaps at
the very end:

```
[bridge] wasm-bindgen module staged     ← post-build hook: NOT served yet
[ort] runtime and SeisBench manifest staged
[seisbench] model weights staged
[py-mt] threaded runtime staged
INFO applying new distribution          ← this is the one to wait for
INFO ✅ success
```

The staging lines can precede the swap by half a minute. Waiting on them and then
reloading gets you the *previous* build — which looks exactly like a fix that did
not work, or a font that still renders as tofu. Use
`wait_for_log(service="web", grep="applying new distribution")`, and if in doubt
check `ls -laT dist/*.wasm` against your edit time.

A wasm build here takes ~15 s to over a minute, and `trunk serve` reads
`index.html` at build *start*, so an edit made mid-compile lands one build later
(the watcher does pick it up — just wait for the second swap).

## Traps

- **Screenshots lie about hover state.** The cursor clock in the top bar only
  appears while the pointer is over the traces.
- **Do not add `pointer-events: auto` to the overlay to make clicking easier.**
  That is the bug this design prevents; use the flag.
- The full a11y contract is asserted by `task test:a11y:browser`
  (`tests/a11y_browser_validate.ts`) — run it after touching the a11y layer.
