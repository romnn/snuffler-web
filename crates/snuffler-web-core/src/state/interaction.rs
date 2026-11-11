//! Transient state of an in-progress pointer interaction or a pending request
//! the session drains into the bridge.

/// Pointer origin and time window captured when a waveform drag starts.
#[derive(Clone, Copy, Debug)]
pub struct ViewDrag {
    /// Pointer x-coordinate in logical pixels.
    pub x: f32,
    /// Pointer y-coordinate in logical pixels.
    pub y: f32,
    /// Visible lower time bound captured at drag start, in epoch seconds.
    pub tmin: f64,
    /// Visible upper time bound captured at drag start, in epoch seconds.
    pub tmax: f64,
    /// Whether the drag translates time without vertical zooming.
    pub pan_only: bool,
}

/// State of an in-progress pick in batch/interactive picking mode (Escape).
#[derive(Clone, Debug, Default)]
pub struct PickingState {
    /// Whether a new pick re-arms after each commit (Escape starts batch mode).
    pub batch_mode: bool,
    /// Time of the pointer press that began the current pick; `None` while the
    /// floating pick merely follows the cursor before a press.
    pub press_time: Option<f64>,
    /// Current floating pick span (epoch seconds); `tmin == tmax` is an instant.
    pub tmin: f64,
    /// Upper bound of the floating pick span, in epoch seconds.
    pub tmax: f64,
    /// Inherited marker attributes carried across re-armed picks.
    pub kind: u8,
    /// Phase name inherited by the next committed marker.
    pub phase: Option<String>,
    /// First-motion polarity inherited by the next committed marker.
    pub polarity: Option<i8>,
    /// Active-event association inherited by the next committed phase pick.
    pub event_hash: Option<String>,
    /// Associated event origin time used by marker-file fallback matching.
    pub event_time: Option<f64>,
    /// The channels of the track under the cursor the pick is scoped to — every channel
    /// gathered onto the row, as Qt's `get_nslc_ids_for_track` returns them.
    /// Empty while no track is under the cursor.
    pub nslc_ids: Vec<(String, String, String, String)>,
}

/// A pending "set active event" request the session drains and sends to the bridge (input
/// handlers set it; they cannot touch the bridge directly).
/// The session converts it to a `SetActiveEventRequest` proto message.
#[derive(Clone, Debug, Default)]
pub struct ActiveEventRequest {
    /// Whether the active event should be cleared instead of replaced.
    pub deactivate: bool,
    /// Stable event identifier when the marker came from a catalog.
    pub event_hash: Option<String>,
    /// Event origin time in epoch seconds.
    pub time: f64,
    /// Optional channel identifying the relevant marker.
    pub nslc: Option<(String, String, String, String)>,
}
