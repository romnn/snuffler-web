//! Settings that survive a reload, snuffler's `~/.pyrocko/snuffler.pf`.
//!
//! Qt reads a handful of user-editable settings at every start (`SnufflerConfig`,
//! config.py:33) and writes them back on change. `localStorage` is the browser's
//! `snuffler.pf` — the ':' command history already lives there — so the same
//! settings persist here, one key per setting exactly as a `.pf` holds one line
//! per setting. The set matches Qt's: demean, the two scale annotations, the
//! scaling mode, show boxes, clip traces, and the chosen visible length (Qt
//! persists the visible-length *table*; the web's four entries are fixed, so the
//! choice is what there is to keep).

use snuffler_web_core::state::{AppState, ScalingBase, ScalingGroup, VisibleLength};

/// One key per setting under this prefix, like a `.pf` line per setting.
const PREFIX: &str = "snuffler.settings.";

/// What the last save wrote, so a frame that changed nothing writes nothing.
#[derive(PartialEq, Eq)]
pub struct Snapshot {
    demean: bool,
    show_scale_ranges: bool,
    show_scale_axes: bool,
    show_boxes: bool,
    clip_traces: bool,
    scaling_group: &'static str,
    scaling_base: &'static str,
    visible_length: &'static str,
}

impl Snapshot {
    fn of(state: &AppState) -> Self {
        Self {
            demean: state.demean,
            show_scale_ranges: state.show_scale_ranges,
            show_scale_axes: state.show_scale_axes,
            show_boxes: state.show_boxes,
            clip_traces: state.clip_traces,
            scaling_group: state.scaling_group.as_str(),
            scaling_base: state.scaling_base.as_str(),
            visible_length: state.visible_length.label(),
        }
    }
}

fn storage() -> Option<web_sys::Storage> {
    // Private-mode browsers and sandboxed frames refuse storage access; the
    // settings still work, they just forget between reloads.
    web_sys::window()?.local_storage().ok().flatten()
}

fn read(storage: &web_sys::Storage, key: &str) -> Option<String> {
    storage.get_item(&format!("{PREFIX}{key}")).ok().flatten()
}

fn read_bool(storage: &web_sys::Storage, key: &str, value: &mut bool) {
    match read(storage, key).as_deref() {
        Some("true") => *value = true,
        Some("false") => *value = false,
        _ => {}
    }
}

/// Applies the stored settings over the defaults, at boot. A key that is
/// missing or unreadable leaves its default alone, so a first visit (and a
/// stale store from an older build) behaves exactly like Qt's defaults.
pub fn restore(state: &mut AppState) {
    let Some(storage) = storage() else {
        return;
    };
    read_bool(&storage, "demean", &mut state.demean);
    read_bool(&storage, "show_scale_ranges", &mut state.show_scale_ranges);
    read_bool(&storage, "show_scale_axes", &mut state.show_scale_axes);
    read_bool(&storage, "show_boxes", &mut state.show_boxes);
    read_bool(&storage, "clip_traces", &mut state.clip_traces);
    if let Some(group) = read(&storage, "scaling_group")
        .as_deref()
        .and_then(ScalingGroup::from_wire)
    {
        state.scaling_group = group;
    }
    if let Some(base) = read(&storage, "scaling_base")
        .as_deref()
        .and_then(ScalingBase::from_wire)
    {
        state.scaling_base = base;
    }
    if let Some(length) = read(&storage, "visible_length")
        .as_deref()
        .and_then(VisibleLength::from_wire)
    {
        state.visible_length = length;
    }
}

/// Writes the settings back when one of them changed since the last save —
/// called once per frame, writing only on change, as Qt saves its config from
/// the menu-item slots that change it.
pub fn save_if_changed(state: &AppState, last_saved: &mut Option<Snapshot>) {
    let snapshot = Snapshot::of(state);
    if last_saved.as_ref() == Some(&snapshot) {
        return;
    }
    let Some(storage) = storage() else {
        return;
    };
    for (key, value) in [
        ("demean", snapshot.demean),
        ("show_scale_ranges", snapshot.show_scale_ranges),
        ("show_scale_axes", snapshot.show_scale_axes),
        ("show_boxes", snapshot.show_boxes),
        ("clip_traces", snapshot.clip_traces),
    ] {
        let _ = storage.set_item(
            &format!("{PREFIX}{key}"),
            if value { "true" } else { "false" },
        );
    }
    for (key, value) in [
        ("scaling_group", snapshot.scaling_group),
        ("scaling_base", snapshot.scaling_base),
        ("visible_length", snapshot.visible_length),
    ] {
        let _ = storage.set_item(&format!("{PREFIX}{key}"), value);
    }
    *last_saved = Some(snapshot);
}
