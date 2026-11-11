//! Cost accounting for the projection, compiled in by the `metrics` feature.
//!
//! Without the feature, [`Recorder`] is a zero-sized no-op with the same methods, so the call
//! sites stay free of `cfg` and make no timing calls into JS.

/// What the projection has cost so far.
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// Tree updates projected.
    pub passes: u32,
    /// Total milliseconds spent inside [`crate::Adapter::update`].
    pub total_ms: f64,
    /// Proxies whose role, name or state was rewritten.
    pub described: u32,
    /// Proxies whose position or size was rewritten.
    pub placed: u32,
}

/// Opaque start-of-pass marker handed from [`Recorder::begin`] to [`Recorder::end_pass`].
///
/// A named type, so the no-op stand-in does not return a bare `()` that would trip
/// `clippy::let_unit_value` at the call site.
#[cfg(feature = "metrics")]
#[derive(Clone, Copy)]
pub(crate) struct PassStart(Option<f64>);

#[cfg(feature = "metrics")]
pub(crate) struct Recorder {
    /// Held rather than looked up per pass: `window.performance` is a JS crossing.
    clock: Option<web_sys::Performance>,
    stats: Stats,
}

#[cfg(feature = "metrics")]
impl Recorder {
    pub fn new() -> Self {
        Self {
            clock: web_sys::window().and_then(|window| window.performance()),
            stats: Stats::default(),
        }
    }

    /// Timestamp of the pass about to run, if a clock is available.
    pub fn begin(&self) -> PassStart {
        PassStart(self.clock.as_ref().map(web_sys::Performance::now))
    }

    pub fn end_pass(&mut self, started: PassStart) {
        let (Some(started), Some(clock)) = (started.0, self.clock.as_ref()) else {
            return;
        };
        self.stats.passes = self.stats.passes.saturating_add(1);
        self.stats.total_ms += clock.now() - started;
    }

    pub fn described(&mut self) {
        self.stats.described = self.stats.described.saturating_add(1);
    }

    pub fn placed(&mut self) {
        self.stats.placed = self.stats.placed.saturating_add(1);
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }
}

#[cfg(not(feature = "metrics"))]
#[derive(Clone, Copy)]
pub(crate) struct PassStart;

#[cfg(not(feature = "metrics"))]
pub(crate) struct Recorder;

#[cfg(not(feature = "metrics"))]
#[expect(
    clippy::unused_self,
    reason = "no-op stand-ins mirror the metrics-enabled `Recorder` API exactly"
)]
impl Recorder {
    pub fn new() -> Self {
        Self
    }

    pub fn begin(&self) -> PassStart {
        PassStart
    }

    pub fn end_pass(&mut self, _started: PassStart) {}

    pub fn described(&mut self) {}

    pub fn placed(&mut self) {}
}
