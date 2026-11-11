//! Green's-function synthetic waveform parameters.
use std::collections::BTreeMap;

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
    pub time_s: f64,
    /// Source north offset in kilometres.
    pub north_km: f64,
    /// Source east offset in kilometres.
    pub east_km: f64,
    /// Source depth in kilometres.
    pub depth_km: f64,
    /// Moment magnitude (the DC, MT and rectangular sources carry one).
    pub magnitude: f64,
    /// Source strike in degrees.
    pub strike_deg: f64,
    /// Source dip in degrees.
    pub dip_deg: f64,
    /// Source rake in degrees.
    pub rake_deg: f64,
    /// Source-time-function duration in seconds.
    pub stf_duration_s: f64,
    /// Source-time-function type: Qt's four choices (seismosizer.py:35).
    pub stf_type: String,
    /// Which of Qt's five source snufflings this run is (seismosizer.py:280).
    pub source_type: String,
    /// `MTSource`: relative moment-tensor components (normalized, then scaled
    /// to the magnitude's moment).
    pub rm6: [f64; 6],
    /// `SFSource`: north/east/down force components in newtons.
    pub forces: [f64; 3],
    /// Rupture length in kilometres (rectangular/dynamic sources).
    pub length_km: f64,
    /// Rupture width in kilometres.
    pub width_km: f64,
    /// Relative nucleation point along strike, -1..1.
    pub nucleation_x: f64,
    /// Relative nucleation point down dip, -1..1.
    pub nucleation_y: f64,
    /// Rupture velocity in m/s (rectangular source).
    pub rupture_velocity_m_s: f64,
    /// Slip in metres (pseudo-dynamic rupture).
    pub slip_m: f64,
    /// Rupture-velocity-to-shear-velocity ratio (pseudo-dynamic rupture).
    pub gamma: f64,
    /// Patches along strike (pseudo-dynamic rupture).
    pub patches_along_strike: u32,
    /// Patches down dip (pseudo-dynamic rupture).
    pub patches_down_dip: u32,
    /// Tapered tractions switch (pseudo-dynamic rupture).
    pub tapered: bool,
    /// Dial values of the source types not currently active, keyed by type.
    /// Qt's five source snufflings are independent dock panels; the unified
    /// panel keeps that by stashing each type's values on every switch.
    saved: BTreeMap<String, SourceValues>,
}

/// One source type's dial values — everything below the shared store and
/// waveform rows (seismosizer.py:280-560).
#[derive(Clone)]
struct SourceValues {
    time_s: f64,
    north_km: f64,
    east_km: f64,
    depth_km: f64,
    magnitude: f64,
    strike_deg: f64,
    dip_deg: f64,
    rake_deg: f64,
    stf_duration_s: f64,
    stf_type: String,
    rm6: [f64; 6],
    forces: [f64; 3],
    length_km: f64,
    width_km: f64,
    nucleation_x: f64,
    nucleation_y: f64,
    rupture_velocity_m_s: f64,
    slip_m: f64,
    gamma: f64,
    patches_along_strike: u32,
    patches_down_dip: u32,
    tapered: bool,
}

impl SourceValues {
    /// Qt's per-panel defaults; they differ only in the MT panel's magnitude
    /// (5 against the others' 6, seismosizer.py:347).
    fn defaults_for(source_type: &str) -> Self {
        let mut values = Config::default().stash();
        if source_type == "MTSource" {
            values.magnitude = 5.0;
        }
        values
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            store: String::new(),
            stores: Vec::new(),
            load_url: String::new(),
            waveform_type: "Displacement [m]".to_owned(),
            time_s: 0.0,
            north_km: 0.0,
            east_km: 0.0,
            depth_km: 10.0,
            magnitude: 6.0,
            strike_deg: 0.0,
            dip_deg: 90.0,
            rake_deg: 0.0,
            stf_duration_s: 0.0,
            stf_type: "half sin".to_owned(),
            source_type: "DCSource".to_owned(),
            // Qt's MTSource defaults: rel Mne = 1, the rest 0.
            rm6: [0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            forces: [1e3, 1e3, 1e3],
            length_km: 10.0,
            width_km: 5.0,
            nucleation_x: 0.0,
            nucleation_y: 0.0,
            rupture_velocity_m_s: 3500.0,
            slip_m: 1.0,
            gamma: 0.8,
            patches_along_strike: 5,
            patches_down_dip: 5,
            tapered: true,
            saved: BTreeMap::new(),
        }
    }
}

impl Config {
    fn stash(&self) -> SourceValues {
        SourceValues {
            time_s: self.time_s,
            north_km: self.north_km,
            east_km: self.east_km,
            depth_km: self.depth_km,
            magnitude: self.magnitude,
            strike_deg: self.strike_deg,
            dip_deg: self.dip_deg,
            rake_deg: self.rake_deg,
            stf_duration_s: self.stf_duration_s,
            stf_type: self.stf_type.clone(),
            rm6: self.rm6,
            forces: self.forces,
            length_km: self.length_km,
            width_km: self.width_km,
            nucleation_x: self.nucleation_x,
            nucleation_y: self.nucleation_y,
            rupture_velocity_m_s: self.rupture_velocity_m_s,
            slip_m: self.slip_m,
            gamma: self.gamma,
            patches_along_strike: self.patches_along_strike,
            patches_down_dip: self.patches_down_dip,
            tapered: self.tapered,
        }
    }

    fn apply(&mut self, values: SourceValues) {
        self.time_s = values.time_s;
        self.north_km = values.north_km;
        self.east_km = values.east_km;
        self.depth_km = values.depth_km;
        self.magnitude = values.magnitude;
        self.strike_deg = values.strike_deg;
        self.dip_deg = values.dip_deg;
        self.rake_deg = values.rake_deg;
        self.stf_duration_s = values.stf_duration_s;
        self.stf_type = values.stf_type;
        self.rm6 = values.rm6;
        self.forces = values.forces;
        self.length_km = values.length_km;
        self.width_km = values.width_km;
        self.nucleation_x = values.nucleation_x;
        self.nucleation_y = values.nucleation_y;
        self.rupture_velocity_m_s = values.rupture_velocity_m_s;
        self.slip_m = values.slip_m;
        self.gamma = values.gamma;
        self.patches_along_strike = values.patches_along_strike;
        self.patches_down_dip = values.patches_down_dip;
        self.tapered = values.tapered;
    }

    /// Switch the active source type, Qt-style: each of Qt's five source
    /// snufflings owns its dial values independently (seismosizer.py:280-560),
    /// so the outgoing type's values are stashed and the incoming type's come
    /// back — or start at that panel's own defaults on its first visit.
    /// Without this, rendering a shared field under another type's narrower
    /// bounds silently clamps the stored value (an SF STF of 80 s would come
    /// back as DC's 50).
    /// A no-op when the type is unchanged.
    pub fn switch_source(&mut self, source_type: &str) {
        if source_type == self.source_type {
            return;
        }
        let outgoing = self.stash();
        self.saved
            .insert(std::mem::take(&mut self.source_type), outgoing);
        let incoming = self
            .saved
            .remove(source_type)
            .unwrap_or_else(|| SourceValues::defaults_for(source_type));
        self.apply(incoming);
        source_type.clone_into(&mut self.source_type);
    }

    /// Serializes parameters for a bridge request.
    #[must_use]
    pub fn params(&self) -> SeismosizerSnufflingParams {
        SeismosizerSnufflingParams {
            store: self.store.clone(),
            waveform_type: self.waveform_type.clone(),
            time_s: self.time_s,
            north_km: self.north_km,
            east_km: self.east_km,
            depth_km: self.depth_km,
            magnitude: self.magnitude,
            strike_deg: self.strike_deg,
            dip_deg: self.dip_deg,
            rake_deg: self.rake_deg,
            stf_duration_s: self.stf_duration_s,
            stf_type: self.stf_type.clone(),
            source_type: self.source_type.clone(),
            relative_mnn: self.rm6[0],
            relative_mee: self.rm6[1],
            relative_mdd: self.rm6[2],
            relative_mne: self.rm6[3],
            relative_mnd: self.rm6[4],
            relative_med: self.rm6[5],
            force_north_n: self.forces[0],
            force_east_n: self.forces[1],
            force_down_n: self.forces[2],
            length_km: self.length_km,
            width_km: self.width_km,
            nucleation_x: self.nucleation_x,
            nucleation_y: self.nucleation_y,
            rupture_velocity_m_s: self.rupture_velocity_m_s,
            slip_m: self.slip_m,
            gamma: self.gamma,
            patches_along_strike: self.patches_along_strike,
            patches_down_dip: self.patches_down_dip,
            tapered: self.tapered,
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

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn switching_source_types_keeps_each_panels_values() {
        let mut config = Config::default();
        config.switch_source("SFSource");
        config.stf_duration_s = 80.0;
        config.switch_source("DCSource");
        // DC opens on its own defaults, not on SF's leftovers — Qt's five
        // snufflings are independent panels (seismosizer.py:280-560).
        assert!(config.stf_duration_s.abs() < 1e-12);
        config.switch_source("SFSource");
        // The 80 s SF duration survives the round trip instead of being
        // clamped to DC's 50 s ceiling.
        assert!((config.stf_duration_s - 80.0).abs() < 1e-12);
    }

    #[test]
    fn the_mt_panel_first_opens_on_qts_own_defaults() {
        let mut config = Config::default();
        config.switch_source("MTSource");
        // Qt's MT panel defaults to magnitude 5 where the others use 6
        // (seismosizer.py:347).
        assert!((config.magnitude - 5.0).abs() < 1e-12);
        config.switch_source("DCSource");
        assert!((config.magnitude - 6.0).abs() < 1e-12);
    }

    #[test]
    fn a_negative_point_depth_survives_a_visit_to_an_extended_source() {
        let mut config = Config {
            depth_km: -5.0,
            ..Config::default()
        };
        // The rectangular panel's depth floor is 0; peeking at it must not
        // destroy the point source's elevated depth.
        config.switch_source("RectangularSource");
        config.switch_source("DCSource");
        assert!((config.depth_km + 5.0).abs() < 1e-12);
    }
}
