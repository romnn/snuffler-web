//! UI/application state for the viewer (everything the user can tweak).

use crate::view::markers::Marker;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalingMode {
    /// Each track auto-scaled to its own min/max.
    Individual,
    /// All visible tracks share one amplitude range.
    Common,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Wiggle,
    Waterfall,
}

impl ViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Wiggle => "wiggle",
            ViewMode::Waterfall => "waterfall",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    Name,
    Reverse,
}

impl SortMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::Reverse => "name_reverse",
        }
    }
}

pub struct AppState {
    /// Visible time window (epoch seconds).
    pub tmin: f64,
    pub tmax: f64,

    /// Filters (Hz). `None` = filter disabled.
    pub highpass: Option<f64>,
    pub lowpass: Option<f64>,
    /// Last value of the (possibly-disabled) sliders, so toggling keeps it.
    pub highpass_hz: f64,
    pub lowpass_hz: f64,
    pub gain: f32,

    pub scaling: ScalingMode,
    pub sort: SortMode,
    pub view_mode: ViewMode,

    /// Rotation of horizontal components, degrees.
    pub rotate_deg: f64,
    /// Deconvolve instrument response (needs response metadata).
    pub remove_response: bool,
    /// Waterfall colormap clip as fractions of each track's power range.
    pub wf_clip: (f32, f32),
    /// Amplitude-spectrum snuffling window open.
    pub show_spectrum: bool,

    /// Vertical track windowing (snuffler's ntracks_shown_max + scroll).
    pub ntracks_shown: usize,
    pub track_offset: usize,

    pub markers: Vec<Marker>,
    /// Next color group applied to new markers (snuffler number keys).
    pub marker_kind: u8,
    /// In-progress span-marker drag (start, current) in epoch seconds.
    pub pending_span: Option<(f64, f64)>,

    pub show_controls: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tmin: 0.0,
            tmax: 1.0,
            highpass: None,
            lowpass: None,
            highpass_hz: 0.1,
            lowpass_hz: 10.0,
            gain: 1.0,
            scaling: ScalingMode::Individual,
            sort: SortMode::Name,
            view_mode: ViewMode::Wiggle,
            rotate_deg: 0.0,
            remove_response: false,
            wf_clip: (0.0, 1.0),
            show_spectrum: false,
            ntracks_shown: 12,
            track_offset: 0,
            markers: Vec::new(),
            marker_kind: 0,
            pending_span: None,
            show_controls: true,
        }
    }
}
