#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct State {
    pub highpass_hz: f32,
    pub lowpass_hz: f32,
    pub gain: f32,
    pub rotate_deg: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            highpass_hz: 0.0,
            lowpass_hz: 0.0,
            gain: 1.0,
            rotate_deg: 0.0,
        }
    }
}
