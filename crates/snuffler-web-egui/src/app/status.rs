//! The top bar: menus, the activity status line, and the right-hand readout.

use std::fmt::Write as _;

use snuffler_web_core::activity::{
    CancellationCapability, JobError, JobHandle, JobRecord, JobState, Operation, Outcome, Progress,
    RuntimeState,
};

use super::App;

/// How much of the right-hand readout the menu row can afford this frame.
struct RightReadout {
    /// Width to keep clear at the right of the row; zero shows nothing at all.
    width: f32,
    /// Whether the build stamp fits beside the frame rate.
    with_stamp: bool,
}

/// The widest the readout can get (the rate is capped at 60), used to reserve a
/// width that does not change as the number does.
const FRAME_STATS_WIDEST: &str = "60 fps";

impl App {
    pub(super) fn show_status_panel(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        total_tracks: usize,
    ) {
        egui::Panel::top("menu").show_inside(ui, |ui| {
            // This row has to stay exactly one line tall whatever it holds: a
            // wrapped label would make the panel taller and push every panel below
            // it down, moving the traces out from under the pointer.
            // The row's contents come and go (the hover-only cursor clock, the PICK
            // badge, a status message of any length), so truncating is what pins
            // the height for any font, message and window width.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.horizontal(|ui| {
                self.main_menus(ui);
                // snuffler's menu-bar ':' button, which toggles the command line.
                if ui
                    .button(egui::RichText::new(":").monospace().strong())
                    .on_hover_text("Show command line.")
                    .clicked()
                {
                    self.session.state.command.toggle();
                }
                ui.separator();
                // Hand the right edge to the build stamp and frame rate before the
                // status message is laid out: a message of any length then truncates
                // within what is left, instead of pushing the readout off the row.
                let readout = Self::right_readout(ui);
                let left = (ui.available_width() - readout.width).max(0.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(left, ui.available_height()),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| self.show_activity(ui, ctx, total_tracks),
                );
                if readout.width > 0.0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.show_frame_stats(ui);
                        if readout.with_stamp {
                            ui.separator();
                            self.show_build_stamp(ui);
                        }
                    });
                }
            });
        });
    }

    /// The Cancel and Restart Python buttons beside the status line, when the
    /// active job or the runtime is in a state the user can act on.
    fn show_recovery_actions(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &RuntimeState,
        active: Option<(JobHandle, &JobRecord)>,
    ) {
        // A failed runtime does not recreate workers on its own (a persistent boot
        // failure would loop forever), so recovery needs a button here.
        if matches!(runtime, RuntimeState::Failed(_)) && ui.small_button("Restart Python").clicked()
        {
            self.session.bridge.restart_runtime();
        }
        if matches!(runtime, RuntimeState::Ready)
            && let Some((handle, record)) = active
        {
            let queued = matches!(record.state(), JobState::Queued);
            let cancellable = queued
                || (record.operation().activity().cancellation
                    != CancellationCapability::NotSupported
                    && !matches!(record.state(), JobState::Cancelling { .. }));
            if cancellable && ui.small_button("Cancel").clicked() {
                let _disposition = self.session.bridge.cancel(handle);
            }
            // Restarting is the recovery of last resort, so it appears only for
            // a cancellation the worker stopped honoring, or a non-cancellable
            // rpc running long enough to look wedged — that one has no Cancel
            // button and would otherwise hold the job lane with no way out
            // short of a page reload.
            let stuck = match record.state() {
                JobState::Cancelling { since_s, .. } => {
                    js_sys::Date::now() / 1000.0 - since_s > 5.0
                }
                JobState::Running { since_s, .. } => {
                    record.operation().activity().cancellation
                        == CancellationCapability::NotSupported
                        && js_sys::Date::now() / 1000.0 - since_s > 30.0
                }
                _ => false,
            };
            if stuck && ui.small_button("Restart Python").clicked() {
                self.session.bridge.restart_runtime();
            }
        }
    }

    /// What the app is doing right now: the status line, the picking badge, and
    /// the time under the pointer.
    fn show_activity(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, total_tracks: usize) {
        // The status line is the one place the app tells you that something
        // finished, failed, or is still running, and none of it is tied to a
        // widget you would be focused on — so it is announced as it changes.
        let (runtime, jobs) = self.session.bridge.activity_snapshot();
        let jobs: &[(JobHandle, JobRecord)] = &jobs;
        let now_s = js_sys::Date::now() / 1000.0;
        if let Some(delay) = next_activity_reveal_delay(jobs, now_s) {
            ctx.request_repaint_after(delay);
        }
        let active = active_job(jobs, now_s);
        let (status, urgency) = show_primary_activity(ui, &runtime, jobs, active, total_tracks);
        self.show_recovery_actions(ui, &runtime, active);
        crate::a11y::announce_changes(ctx, &status, urgency);
        if let Some(error) = self.session.bridge.diagnostic() {
            ui.separator();
            let label = ui.colored_label(
                egui::Color32::from_rgb(0xC0, 0x60, 0x20),
                format!("⚠ {error}"),
            );
            let label = match error.traceback() {
                Some(traceback) => label.on_hover_text(traceback),
                None => label,
            };
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Urgent);
        }
        // Processing failures the view survived (a filter or restitution that
        // threw, drawn unprocessed). Snuffler lets these exceptions reach the
        // user; here the bridge sends them with the view they affected, and the
        // status line is where the user is told rather than the worker console.
        let notices = self
            .session
            .bridge
            .view()
            .map(|view| view.meta.notices.clone())
            .unwrap_or_default();
        if let Some(first) = notices.first() {
            ui.separator();
            let extra = notices.len().saturating_sub(1);
            let text = if extra > 0 {
                format!("⚠ {first} (+{extra} more)")
            } else {
                format!("⚠ {first}")
            };
            let label = ui.colored_label(egui::Color32::from_rgb(0xC0, 0x80, 0x20), text);
            let label = if extra > 0 {
                label.on_hover_text(notices.join("\n"))
            } else {
                label
            };
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
        }
        // Qt's passband_check posts this to the window's permanent status area
        // (pile_viewer.py:4023) — not to the controls panel, which `c` hides —
        // and says what the app did about it: the highpass is dropped from
        // processing while the corners stay inverted.
        if let (Some(hp), Some(lp)) = (self.session.state.highpass, self.session.state.lowpass)
            && hp >= lp
        {
            ui.separator();
            let label = ui.colored_label(
                egui::Color32::from_rgb(0xCF, 0x8E, 0x00),
                "⚠ Corner frequency of highpass greater than corner frequency of lowpass. \
                 Highpass deactivated.",
            );
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
        }
        if self.session.state.picking.is_some() {
            ui.separator();
            ui.colored_label(
                egui::Color32::from_rgb(0xE0, 0x50, 0x50),
                "◉ PICK (Esc to exit)",
            )
            .on_hover_text(
                "Batch picking: click/drag on a trace to place a pick; number/F keys \
                 set kind/phase and carry to the next pick.",
            );
        }
        // The hovered marker's metadata, as snuffler posts `hoover_message()` to
        // its status bar (pile_viewer.py:2200) — an event's origin fields or a
        // pick's polarity, readable without opening the marker table.
        let hover_msg = self
            .session
            .state
            .hovered_marker
            .and_then(|id| self.session.state.markers.by_id(id))
            .map(snuffler_web_core::markers::Marker::hover_message)
            .filter(|msg| !msg.is_empty());
        if let Some(msg) = hover_msg {
            ui.separator();
            let label = ui.label(msg);
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
        }
        self.show_marker_hit_overload(ui, ctx);
        self.show_cursor_time(ui, ctx);
    }

    fn show_marker_hit_overload(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(count) = self.session.state.marker_hit_overload else {
            return;
        };
        ui.separator();
        let label = ui.label(format!(
            "{count} marker endpoints under the pointer; zoom in to select one"
        ));
        crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
    }

    /// How much of the build stamp and frame rate the right of the menu row can
    /// take without crowding out what is to their left.
    ///
    /// Measured rather than assumed: the menus are as wide as the font makes them,
    /// the stamp is a timestamp with a zone or just `dev build`, and the window is
    /// any size, so any fixed threshold is wrong for some combination of them.
    fn right_readout(ui: &egui::Ui) -> RightReadout {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let text_width = |text: String| {
            ui.painter()
                .layout_no_wrap(text, font.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        };
        let gap = ui.spacing().item_spacing.x;
        // Reserved for the widest rate rather than the current one, so the status
        // beside it does not resize when the first measurement lands or the number
        // gains a digit. `6.0` is the width egui gives a vertical separator.
        let rate = text_width(FRAME_STATS_WIDEST.to_owned()) + gap * 2.0;
        let stamp = text_width(crate::view::about::build_stamp()) + 6.0 + gap * 2.0;
        // What the menus and the status keep no matter what: below it the readout
        // is the part worth dropping, and dropping it is what keeps it from
        // painting over them.
        let status_min = ui.text_style_height(&egui::TextStyle::Body) * 8.0;
        let spare = ui.available_width() - status_min;
        if spare >= rate + stamp {
            RightReadout {
                width: rate + stamp,
                with_stamp: true,
            }
        } else if spare >= rate {
            // The stamp is by far the longer of the two and never changes within a
            // build; the rate is short and is the one worth watching.
            RightReadout {
                width: rate,
                with_stamp: false,
            }
        } else {
            RightReadout {
                width: 0.0,
                with_stamp: false,
            }
        }
    }

    /// When the published build was built, and the way into the About window it
    /// is a one-line summary of.
    fn show_build_stamp(&mut self, ui: &mut egui::Ui) {
        let stamp = egui::RichText::new(crate::view::about::build_stamp())
            .monospace()
            .weak();
        if ui
            .add(egui::Label::new(stamp).sense(egui::Sense::click()))
            .on_hover_text(crate::view::about::build_tooltip())
            .clicked()
        {
            self.session.state.show_about = true;
        }
    }

    fn show_frame_stats(&self, ui: &mut egui::Ui) {
        let Some(fps) = self.frame_stats_shown_fps else {
            return;
        };
        ui.label(egui::RichText::new(format!("{fps} fps")).monospace().weak())
            .on_hover_text(
                "Frames per second this machine could sustain, from egui's CPU time \
                 per frame. Capped at 60; the view only redraws when something changes.",
            );
    }

    fn show_cursor_time(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(position) = ctx.pointer_hover_pos() else {
            return;
        };
        // The whole rect, not just its horizontal span: the controls sit directly
        // below the traces and share their x range, so testing x alone would show
        // the clock while the pointer is on a slider down there.
        let rect = self.last_waveform_rect;
        if !rect.is_positive() || !rect.contains(position) {
            return;
        }
        let fraction = f64::from(((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
        let time = self.session.state.tmin
            + fraction * (self.session.state.tmax - self.session.state.tmin);
        ui.separator();
        ui.monospace(snuffler_web_core::timeaxis::format_full(time));
    }
}

fn active_job(jobs: &[(JobHandle, JobRecord)], now_s: f64) -> Option<(JobHandle, &JobRecord)> {
    jobs.iter()
        .filter(|(_, record)| {
            !record.state().is_terminal() && record.feedback_delay(now_s).is_none()
        })
        .max_by_key(|(handle, record)| {
            (
                !record.operation().activity().secondary_feedback,
                matches!(
                    record.state(),
                    JobState::Running { .. } | JobState::Dispatching | JobState::Cancelling { .. }
                ),
                handle.id().get(),
            )
        })
        .map(|(handle, record)| (*handle, record))
}

fn next_activity_reveal_delay(
    jobs: &[(JobHandle, JobRecord)],
    now_s: f64,
) -> Option<std::time::Duration> {
    jobs.iter()
        .filter(|(_, record)| !record.state().is_terminal())
        .filter_map(|(_, record)| record.feedback_delay(now_s))
        .min()
}

// One pass: an outcome is resolved by any later submission of the same
// operation, so the newest record per operation decides.
fn newest_per_operation(jobs: &[(JobHandle, JobRecord)]) -> Vec<(u64, &JobRecord)> {
    let mut newest: std::collections::HashMap<Operation, (u64, &JobRecord)> =
        std::collections::HashMap::new();
    for (handle, record) in jobs {
        let id = handle.id().get();
        let entry = newest.entry(record.operation()).or_insert((id, record));
        if id > entry.0 {
            *entry = (id, record);
        }
    }
    newest.into_values().collect()
}

fn unresolved_failure(jobs: &[(JobHandle, JobRecord)]) -> Option<&JobError> {
    newest_per_operation(jobs)
        .into_iter()
        .filter_map(|(id, record)| match record.state() {
            JobState::Done(Outcome::Failed(error)) => Some((id, error)),
            _ => None,
        })
        .max_by_key(|(id, _)| *id)
        .map(|(_, error)| error)
}

/// A partial or lost terminal outcome no later submission superseded. These are
/// not failures — committed work remains installed — but staying silent about
/// them would leave the user with 3 of 400 files and a status line that says
/// only "Ready".
fn unresolved_stop_notice(jobs: &[(JobHandle, JobRecord)]) -> Option<String> {
    newest_per_operation(jobs)
        .into_iter()
        .filter_map(|(id, record)| {
            let label = record.operation().activity().label;
            match record.state() {
                // `skipped` is a lower bound: a lazy directory iteration only
                // knows one more page existed, not how many entries it held.
                JobState::Done(Outcome::Stopped { committed, skipped }) => Some((
                    id,
                    format!("{label} stopped: {committed} committed, ≥{skipped} skipped"),
                )),
                JobState::Done(Outcome::Lost(_)) => {
                    Some((id, format!("{label} was interrupted by a Python restart")))
                }
                _ => None,
            }
        })
        .max_by_key(|(id, _)| *id)
        .map(|(_, text)| text)
}

fn active_job_text(record: &JobRecord, queued: usize) -> String {
    let mut text = record.operation().activity().label.to_owned();
    if let Some(detail) = record.detail() {
        text.push_str(" · ");
        text.push_str(detail);
    }
    match record.state() {
        JobState::Running {
            progress: Progress::Fraction {
                fraction, phase, ..
            },
            ..
        } => {
            text = format!(
                "{phase} · {:.0}%",
                (fraction * 100.0).round().clamp(0.0, 100.0)
            );
        }
        JobState::Running {
            progress: Progress::Count { done, total, phase },
            ..
        } => {
            text = total.map_or_else(
                || format!("{phase} · {done}"),
                |total| format!("{phase} · {done}/{total}"),
            );
        }
        JobState::Running {
            since_s,
            progress: Progress::Indeterminate,
            ..
        } => {
            let elapsed = (js_sys::Date::now() / 1000.0 - since_s).max(0.0);
            if elapsed >= 5.0 {
                text = format!("{text} · still working ({elapsed:.0}s)");
            }
        }
        _ => {}
    }
    if matches!(record.state(), JobState::Cancelling { .. }) {
        text = format!("Cancelling {text}…");
    }
    if queued > 0 {
        let _ = write!(text, " · {queued} queued");
    }
    text
}

fn show_primary_activity(
    ui: &mut egui::Ui,
    runtime: &RuntimeState,
    jobs: &[(JobHandle, JobRecord)],
    active: Option<(JobHandle, &JobRecord)>,
    total_tracks: usize,
) -> (egui::Response, crate::a11y::Urgency) {
    match runtime {
        RuntimeState::Booting => {
            ui.spinner();
            (
                ui.label("Starting Python (Pyodide)…"),
                crate::a11y::Urgency::Polite,
            )
        }
        RuntimeState::Restarting => {
            ui.spinner();
            (
                ui.label("Restarting Python (Pyodide)…"),
                crate::a11y::Urgency::Urgent,
            )
        }
        RuntimeState::Failed(error) => {
            let label = ui.colored_label(egui::Color32::RED, format!("Error: {error}"));
            let label = error
                .traceback()
                .map_or(label.clone(), |traceback| label.on_hover_text(traceback));
            (label, crate::a11y::Urgency::Urgent)
        }
        RuntimeState::Ready => {
            if let Some((_, record)) = active {
                ui.spinner();
                let queued = jobs
                    .iter()
                    .filter(|(_, record)| matches!(record.state(), JobState::Queued))
                    .count();
                return (
                    ui.label(active_job_text(record, queued)),
                    crate::a11y::Urgency::Polite,
                );
            }
            if let Some(error) = unresolved_failure(jobs) {
                let label = ui.colored_label(egui::Color32::RED, format!("Error: {error}"));
                let label = match error {
                    JobError::Bridge(error) => error
                        .traceback()
                        .map_or(label.clone(), |traceback| label.on_hover_text(traceback)),
                    JobError::Rejected(_) | JobError::RuntimeLost => label,
                };
                return (label, crate::a11y::Urgency::Urgent);
            }
            if let Some(notice) = unresolved_stop_notice(jobs) {
                return (
                    ui.colored_label(egui::Color32::from_rgb(0xC0, 0x60, 0x20), notice),
                    crate::a11y::Urgency::Urgent,
                );
            }
            (
                ui.label(format!("Ready · {total_tracks} traces")),
                crate::a11y::Urgency::Polite,
            )
        }
    }
}
