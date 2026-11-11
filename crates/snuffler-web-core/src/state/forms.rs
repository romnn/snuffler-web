//! What the download, catalog and live dialogs hold between openings.

use crate::bridge::{LIVE_FOLLOW_SECONDS, LIVE_RETAIN_SECONDS};

/// The waveform-download dialog: an FDSN dataselect query.
pub struct DownloadForm {
    /// Selected FDSN data-center preset.
    pub datacenter: String,
    /// Optional custom FDSN endpoint overriding the preset.
    pub base_url: String,
    /// Network selector.
    pub net: String,
    /// Station selector.
    pub sta: String,
    /// Location selector.
    pub loc: String,
    /// Channel selector.
    pub cha: String,
    /// Interval start in UTC text form.
    pub start: String,
    /// Interval end in UTC text form.
    pub end: String,
    /// Station search by radius around the active event (Qt's primary download
    /// workflow), instead of by code selectors alone.
    pub around_event: bool,
    /// Radius of the around-event station search, in degrees.
    pub radius_deg: f64,
}

impl Default for DownloadForm {
    fn default() -> Self {
        // Pre-filled with a real example: the 2023-02-06 Turkey M7.8 at GE.WLF.
        Self {
            datacenter: "geofon".into(),
            base_url: String::new(),
            net: "GE".into(),
            sta: "WLF".into(),
            loc: "*".into(),
            cha: "BH?".into(),
            start: "2023-02-06T01:17:00".into(),
            end: "2023-02-06T01:47:00".into(),
            around_event: false,
            radius_deg: 5.0,
        }
    }
}

/// The catalog-search dialog: an FDSN event query.
pub struct CatalogForm {
    /// Selected event-catalog data-center preset.
    pub datacenter: String,
    /// Optional custom event-catalog endpoint overriding the preset.
    pub base_url: String,
    /// Minimum magnitude.
    pub min_magnitude: f64,
    /// Interval start in UTC text form.
    pub start: String,
    /// Interval end in UTC text form.
    pub end: String,
    /// Request full `QuakeML` with moment tensors (beachball + Kagan) rather than the
    /// lightweight text summary.
    pub with_moment_tensors: bool,
}

impl Default for CatalogForm {
    fn default() -> Self {
        Self {
            // GEOFON, as Qt's catalog search defaults (catalogs.py:56).
            datacenter: "geofon".into(),
            base_url: String::new(),
            // 0, as Qt defaults; a 6.0 floor silently hides most seismicity.
            min_magnitude: 0.0,
            start: "2023-02-06".into(),
            end: "2023-02-07".into(),
            with_moment_tensors: true,
        }
    }
}

/// The live `SeedLink` dialog.
pub struct SeedlinkForm {
    /// Server host.
    pub host: String,
    /// Server port.
    pub port: String,
    /// Network selector.
    pub net: String,
    /// Station selector.
    pub sta: String,
    /// Channel selectors.
    pub selectors: String,
    /// Follow-viewport width in seconds (Qt's `--follow N`).
    pub follow_s: f64,
    /// Seconds of live data kept before older samples are discarded.
    /// The wasm32 heap is one bounded block, so the buffer needs *a* bound, but the
    /// number is the user's to set.
    pub retain_s: f64,
}

impl Default for SeedlinkForm {
    fn default() -> Self {
        Self {
            host: "rtserve.earthscope.org".into(),
            port: "18000".into(),
            net: "IU".into(),
            sta: "ANMO".into(),
            selectors: "BH?".into(),
            follow_s: LIVE_FOLLOW_SECONDS,
            retain_s: LIVE_RETAIN_SECONDS,
        }
    }
}
