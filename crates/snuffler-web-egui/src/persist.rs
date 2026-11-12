//! Settings that survive a reload, snuffler's `~/.pyrocko/snuffler.pf`.
//!
//! Qt reads a handful of user-editable settings at every start (`SnufflerConfig`,
//! config.py:33) and writes them back on change.
//! `localStorage` is the browser's `snuffler.pf` — the ':' command history already lives
//! there — so the same settings persist here, one key per setting exactly as a `.pf` holds
//! one line per setting.
//! The set matches Qt's: demean, the two scale annotations, the scaling mode, show boxes,
//! clip traces, and the chosen visible length (Qt persists the visible-length *table*; the
//! web's entries are fixed, so the choice is what there is to keep).
//!
//! Appearance settings specific to this frontend include the wiggle theme
//! and light/dark preference.
//! An appearance that resets on reload is
//! the one thing a user notices immediately — so they are kept here rather than in eframe's
//! own storage, alongside everything else the reload restores.

use snuffler_web_core::state::{AppState, ScalingBase, ScalingGroup, VisibleLength};
use snuffler_web_core::theme::WiggleTheme;

/// One key per setting under this prefix, like a `.pf` line per setting.
const PREFIX: &str = "snuffler.settings.";

/// What [`restore`] recovered that has no home in [`AppState`].
pub struct Restored {
    /// The light/dark preference to apply, which is the user's own choice and not
    /// necessarily what egui ends up holding: a canvas-pinning wiggle theme
    /// overrides it for as long as it is selected (see
    /// `WiggleTheme::pinned_polarity`).
    pub theme_preference: egui::ThemePreference,
}

/// What the last save wrote, so a frame that changed nothing writes nothing.
#[derive(PartialEq, Eq)]
pub struct Snapshot {
    demean: bool,
    show_scale_ranges: bool,
    show_scale_axes: bool,
    show_boxes: Option<bool>,
    clip_traces: bool,
    scaling_group: &'static str,
    scaling_base: &'static str,
    visible_length: &'static str,
    wiggle_theme: &'static str,
    theme_preference: &'static str,
}

impl Snapshot {
    fn of(state: &AppState, theme_preference: egui::ThemePreference) -> Self {
        Self {
            demean: state.demean,
            show_scale_ranges: state.show_scale_ranges,
            show_scale_axes: state.show_scale_axes,
            show_boxes: state.show_boxes,
            clip_traces: state.clip_traces,
            scaling_group: state.scaling_group.as_str(),
            scaling_base: state.scaling_base.as_str(),
            visible_length: state.visible_length.label(),
            wiggle_theme: state.wiggle_theme.label(),
            theme_preference: theme_key(theme_preference),
        }
    }
}

/// The stored spelling of a light/dark preference.
///
/// Lowercase words rather than egui's `Debug`, so the store stays readable and survives an
/// egui rename.
fn theme_key(preference: egui::ThemePreference) -> &'static str {
    match preference {
        egui::ThemePreference::Dark => "dark",
        egui::ThemePreference::Light => "light",
        egui::ThemePreference::System => "system",
    }
}

/// The inverse of [`theme_key`].
fn theme_from_wire(value: &str) -> Option<egui::ThemePreference> {
    match value {
        "dark" => Some(egui::ThemePreference::Dark),
        "light" => Some(egui::ThemePreference::Light),
        "system" => Some(egui::ThemePreference::System),
        _ => None,
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

/// Restores a setting that carries "no choice made" as a third state, leaving it
/// alone when the store has nothing to say.
fn read_opt_bool(storage: &web_sys::Storage, key: &str, value: &mut Option<bool>) {
    match read(storage, key).as_deref() {
        Some("true") => *value = Some(true),
        Some("false") => *value = Some(false),
        _ => {}
    }
}

fn read_bool(storage: &web_sys::Storage, key: &str, value: &mut bool) {
    match read(storage, key).as_deref() {
        Some("true") => *value = true,
        Some("false") => *value = false,
        _ => {}
    }
}

/// Applies the stored settings over the defaults, at boot.
///
/// A key that is missing or unreadable leaves its default alone, so a first visit (and a
/// stale store from an older build) uses the application's defaults.
pub fn restore(state: &mut AppState) -> Restored {
    // Following the system is what a browser app does absent a choice, but the
    // default wiggle theme may pin it light anyway — that is the caller's to
    // resolve, since only it holds the egui context.
    let mut restored = Restored {
        theme_preference: egui::ThemePreference::System,
    };
    let Some(storage) = storage() else {
        return restored;
    };
    read_bool(&storage, "demean", &mut state.demean);
    read_bool(&storage, "show_scale_ranges", &mut state.show_scale_ranges);
    read_bool(&storage, "show_scale_axes", &mut state.show_scale_axes);
    read_opt_bool(&storage, "show_boxes", &mut state.show_boxes);
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
    if let Some(theme) = read(&storage, "wiggle_theme")
        .as_deref()
        .and_then(WiggleTheme::from_wire)
        .filter(|theme| theme.enabled())
    {
        state.wiggle_theme = theme;
    }
    if let Some(preference) = read(&storage, "theme_preference")
        .as_deref()
        .and_then(theme_from_wire)
    {
        restored.theme_preference = preference;
    }
    restored
}

/// Writes the settings back when one of them changed since the last save —
/// called once per frame, writing only on change, as Qt saves its config from
/// the menu-item slots that change it.
///
/// `theme_preference` is the user's own light/dark choice, which is not always
/// what egui is currently drawing in: a canvas-pinning wiggle theme takes the
/// frontend light while it is selected, and storing *that* would quietly discard
/// a dark choice the moment the Snuffler Qt theme was tried on.
pub fn save_if_changed(
    state: &AppState,
    theme_preference: egui::ThemePreference,
    last_saved: &mut Option<Snapshot>,
) {
    let snapshot = Snapshot::of(state, theme_preference);
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
        ("clip_traces", snapshot.clip_traces),
    ] {
        let _ = storage.set_item(
            &format!("{PREFIX}{key}"),
            if value { "true" } else { "false" },
        );
    }
    // A setting the user has not touched is stored as absent rather than as its
    // current value, so it keeps following the canvas default instead of freezing
    // at whatever the canvas happened to be the first time the settings were saved.
    match snapshot.show_boxes {
        Some(value) => {
            let _ = storage.set_item(
                &format!("{PREFIX}show_boxes"),
                if value { "true" } else { "false" },
            );
        }
        None => {
            let _ = storage.remove_item(&format!("{PREFIX}show_boxes"));
        }
    }
    for (key, value) in [
        ("scaling_group", snapshot.scaling_group),
        ("scaling_base", snapshot.scaling_base),
        ("visible_length", snapshot.visible_length),
        ("wiggle_theme", snapshot.wiggle_theme),
        ("theme_preference", snapshot.theme_preference),
    ] {
        let _ = storage.set_item(&format!("{PREFIX}{key}"), value);
    }
    *last_saved = Some(snapshot);
}
