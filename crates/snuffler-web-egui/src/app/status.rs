//! The menu bar, activity feedback, cursor clock, and build readouts.

use std::fmt::Write as _;

use snuffler_web_core::activity::{
    CancellationCapability, JobError, JobHandle, JobRecord, JobState, Operation, Outcome, Progress,
    RuntimeState,
};

use snuffler_web_core::state::AppState;
use snuffler_web_core::theme::WiggleTheme;

use super::App;

impl App {
    pub(super) fn show_status_panel(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        total_tracks: usize,
    ) {
        egui::Panel::top("menu").show_inside(ui, |ui| {
            // Truncation keeps progress messages from changing the row height.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.horizontal(|ui| {
                if ui.available_width() < 1000.0 {
                    ui.menu_button("Menu", |ui| self.main_menus(ui));
                } else {
                    self.main_menus(ui);
                }
                // Snuffler's menu-bar ':' button toggles the command line.
                if ui
                    .button(egui::RichText::new(":").monospace().strong())
                    .on_hover_text("Show command line.")
                    .clicked()
                {
                    self.session.state.command.toggle();
                }
                ui.separator();
                self.show_cursor_time(ui, ctx);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
                    self.show_activity(ui, ctx, total_tracks);
                });
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

    /// What the app is doing right now, including processing errors and recovery actions.
    fn show_activity(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, total_tracks: usize) {
        // The status line is the one place the app tells you that something
        // finished, failed, or is still running, and none of it is tied to a
        // widget you would be focused on — so it is announced as it changes.
        let snapshot = self.session.bridge.activity_snapshot();
        let runtime = snapshot.runtime;
        let jobs: &[(JobHandle, JobRecord)] = &snapshot.jobs;
        let now_s = js_sys::Date::now() / 1000.0;
        if let Some(delay) = next_activity_reveal_delay(jobs, now_s) {
            ctx.request_repaint_after(delay);
        }
        let active = active_job(jobs, now_s);
        // Reserve recovery buttons at the right edge before a long message truncates.
        ui.push_id("recovery_actions", |ui| {
            self.show_recovery_actions(ui, &runtime, active);
        });
        let (status, urgency) = ui
            .push_id("primary_activity", |ui| {
                show_primary_activity(ui, &runtime, jobs, active, total_tracks)
            })
            .inner;
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
        // Processing failures the view survived (a filter or restitution that threw, drawn
        // unprocessed).
        // Snuffler lets these exceptions reach the user; here the bridge sends them with the view
        // they affected, and the status line is where the user is told rather than the worker
        // console.
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
    }

    /// A permanent row below the time axis keeps marker hovers from moving the tracks.
    pub(super) fn show_marker_info_panel(&self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::bottom("marker_info").show_inside(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.horizontal(|ui| {
                ui.set_min_height(ui.text_style_height(&egui::TextStyle::Body));
                self.show_marker_hit_overload(ui, &ctx);
                self.show_marker_hover(ui, &ctx);
            });
        });
    }

    fn show_marker_hover(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
            let label = ui.label(&msg).on_hover_text(&msg);
            crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
        }
    }

    fn show_marker_hit_overload(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(count) = self.session.state.marker_hit_overload else {
            return;
        };
        let label = ui.label(format!(
            "{count} marker endpoints under the pointer; zoom in to select one"
        ));
        crate::a11y::announce_changes(ctx, &label, crate::a11y::Urgency::Polite);
    }

    /// Small theme, build and performance readouts at the right of the marker controls.
    pub(super) fn show_build_readout(ui: &mut egui::Ui, fps: Option<u32>, state: &mut AppState) {
        let stamp = crate::view::about::build_stamp();
        let font = egui::FontId::monospace(11.0);
        let width = ui
            .painter()
            .layout_no_wrap(format!("{stamp}  60 fps"), font, egui::Color32::PLACEHOLDER)
            .size()
            .x
            + ui.spacing().item_spacing.x * 2.0
            + 6.0;
        // Keep the existing controls on their row when the window is narrow.
        if ui.available_width() < width {
            return;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            if let Some(fps) = fps {
                ui.label(
                    egui::RichText::new(format!("{fps} fps"))
                        .monospace()
                        .size(11.0)
                        .weak(),
                );
                ui.separator();
            }
            let response = ui
                .push_id("build_version", |ui| {
                    readout_link(ui, egui::RichText::new(stamp).monospace().size(11.0))
                })
                .inner;
            if response.clicked() {
                state.show_about = true;
            }
            if state.wiggle_theme != WiggleTheme::SnufflerQt {
                let text = "Switch to the original Snuffler theme";
                let note_width = ui
                    .painter()
                    .layout_no_wrap(
                        text.to_owned(),
                        egui::FontId::proportional(11.0),
                        egui::Color32::PLACEHOLDER,
                    )
                    .size()
                    .x
                    + ui.spacing().item_spacing.x * 2.0
                    + 6.0;
                if ui.available_width() >= note_width {
                    ui.separator();
                    if ui
                        .push_id("original_theme", |ui| {
                            readout_link(ui, egui::RichText::new(text).size(11.0))
                        })
                        .inner
                        .clicked()
                    {
                        state.wiggle_theme = WiggleTheme::SnufflerQt;
                    }
                }
            }
        });
    }

    fn show_cursor_time(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Test the full rect so moving over a control does not update the clock.
        let rect = self.last_waveform_rect;
        if let Some(position) = ctx.pointer_hover_pos()
            && rect.is_positive()
            && rect.contains(position)
        {
            let fraction = f64::from(((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
            self.last_cursor_time = Some(
                self.session.state.tmin
                    + fraction * (self.session.state.tmax - self.session.state.tmin),
            );
        }
        let text = self.last_cursor_time.map_or_else(
            || "---------- --:--:--.--- UTC".to_owned(),
            |time| {
                format!(
                    "{} UTC",
                    snuffler_web_core::timeaxis::format_timestamp(time)
                )
            },
        );
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let width = ui
            .painter()
            .layout_no_wrap(
                "0000-00-00 00:00:00.000 UTC".to_owned(),
                font,
                egui::Color32::PLACEHOLDER,
            )
            .size()
            .x;
        ui.add_sized(
            egui::vec2(width, ui.spacing().interact_size.y),
            egui::Label::new(egui::RichText::new(text).monospace()).truncate(),
        )
        .on_hover_text("UTC time at the last pointer position over the tracks.");
    }
}

fn readout_link(ui: &mut egui::Ui, text: egui::RichText) -> egui::Response {
    let response = ui
        .add(egui::Label::new(text.weak()).sense(egui::Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() || response.has_focus() {
        ui.painter().line_segment(
            [response.rect.left_bottom(), response.rect.right_bottom()],
            egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
        );
    }
    response
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

/// A partial or lost terminal outcome no later submission superseded.
///
/// These are not failures — committed work remains installed — but staying silent about them
/// would leave the user with 3 of 400 files and a status line that says only "Ready".
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
