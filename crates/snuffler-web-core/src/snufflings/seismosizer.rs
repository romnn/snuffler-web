//! Green's-function synthetic waveform parameters.
use crate::bridge::SeismosizerSnufflingParams;

/// Configuration for pyrocko's Seismosizer snuffling.
pub struct Config {
    /// Selected mounted GF-store label ("GF Store: <id>"), and the available set.
    pub store: String,
    /// Mounted Green's-function stores available to the selector.
    pub stores: Vec<String>,
    /// HTTP base URL to mount a GF store from (an alternative to the directory
    /// picker; the base must serve a `.manifest` of its files).
    pub load_url: String,
    /// Requested displacement, velocity, or acceleration unit.
    pub waveform_type: String,
    /// Source-time offset in seconds.
    pub time: f64,
    /// Source north offset in kilometres.
    pub north_km: f64,
    /// Source east offset in kilometres.
    pub east_km: f64,
    /// Source depth in kilometres.
    pub depth_km: f64,
    /// Moment magnitude of the double-couple source.
    pub magnitude: f64,
    /// Source strike in degrees.
    pub strike: f64,
    /// Source dip in degrees.
    pub dip: f64,
    /// Source rake in degrees.
    pub rake: f64,
    /// Source-time-function duration in seconds.
    pub stf_duration: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            store: String::new(),
            stores: Vec::new(),
            load_url: String::new(),
            waveform_type: "Displacement [m]".to_owned(),
            time: 0.0,
            north_km: 0.0,
            east_km: 0.0,
            depth_km: 10.0,
            magnitude: 6.0,
            strike: 0.0,
            dip: 90.0,
            rake: 0.0,
            stf_duration: 0.0,
        }
    }
}

impl Config {
    /// Serializes parameters for a bridge request.
    #[must_use]
    pub fn params(&self) -> SeismosizerSnufflingParams {
        SeismosizerSnufflingParams {
            seis_store: self.store.clone(),
            seis_waveform_type: self.waveform_type.clone(),
            seis_time: self.time,
            seis_north_km: self.north_km,
            seis_east_km: self.east_km,
            seis_depth_km: self.depth_km,
            seis_magnitude: self.magnitude,
            seis_strike: self.strike,
            seis_dip: self.dip,
            seis_rake: self.rake,
            seis_stf_duration: self.stf_duration,
        }
    }

    /// Register a mounted GF store (from "Add GF Store…") and select it.
    pub fn add_store(&mut self, label: String) {
        if !self.stores.contains(&label) {
            self.stores.push(label.clone());
        }
        self.store = label;
    }

    /// Returns the editable remote store base URL.
    #[must_use]
    pub fn load_url(&self) -> String {
        self.load_url.clone()
    }
}
