# Bridge borrow-safety and wasm-worker review

## Review target

- Snuffler Web head: `de1723205bf6b2c5d6455eaa704d078ff390d7b8` plus the current uncommitted tree.
- Application scope: `crates/snuffler-web-core/src/bridge/`,
  `crates/snuffler-web-egui/src/app.rs`, and every `Bridge::view` call site.
- Egui fork scope: `/Volumes/T7/dev/eframe/crates/epaint/src/mutex.rs` at fork head
  `242294f189416b958304231d4d009beee2aa62c4` plus its current uncommitted patch.
- The live files are authoritative if a diff and source disagree.
- Reviewers are read-only and must not modify either worktree.

## Domain goal

The waveform frontend must draw and export immutable view snapshots while asynchronous bridge jobs
continue to update runtime state. A reader must never keep a dynamic `RefCell` borrow alive across an
operation that mutates bridge state. Expensive export work should run in the wasm rayon pool without
moving browser- or egui-main-thread-only state to workers.

## Established failures to re-verify

1. Moving `egui::Context` and full SVG generation into `rayon::spawn` caused epaint's contended
   debug lock to call unsupported wasm `std::time::Instant` and abort the page.
2. Calling `Bridge::finish_local` while a `Bridge::view() -> Ref<ViewData>` guard remained alive
   caused `RefCell already borrowed` and aborted the page.
3. The current safe workaround generates SVG on the main thread and sends only the owned SVG string
   to rayon for resvg rasterization.
4. The epaint fork patch keeps timed deadlock detection on native debug builds and uses ordinary
   blocking locks on `wasm32-unknown-unknown`.

## Questions

1. Can the public bridge API make the escaped-borrow failure impossible at compile time without
   copying the large waveform sample buffer?
2. Is storing `Arc<ViewData>` and returning an owned `Arc<ViewData>` snapshot the smallest sound
   seam, or is another ownership shape materially better?
3. Which other public APIs leak dynamic borrow guards or references tied to `Inner`?
4. Does the epaint conditional-lock patch preserve native diagnostics and make wasm worker
   contention sound, including atomics-enabled workers?
5. After the fork fix, is worker-side SVG generation with a cloned `egui::Context` architecturally
   sound and beneficial, or should egui text measurement remain on the main thread?
6. What executable browser and compile-time regressions are required to prove the chosen boundary?

## Required output

Lead with one verdict: `sound`, `sound with repairs`, or `do not ship`.

For every finding, provide severity, exact `file:line` evidence, a reachable failure scenario, and
the smallest structural repair. Distinguish confirmed findings from suspected risks. Explicitly
state which tempting abstractions would add complexity without stronger guarantees.
