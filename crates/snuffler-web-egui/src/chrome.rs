//! The frontend's own chrome colours: egui's light and dark visuals, tuned for
//! contrast and for the canvas they frame.
//!
//! egui's stock light theme is built for dense tool windows: grey-80 text on
//! grey-248 panels, grey-60 button labels on grey-230 buttons.
//! Next to a waveform canvas somebody reads for hours it looks washed out, and it
//! is neutral where the canvas is slightly blue-cool.
//! Both palettes here sit on the same cool neutral ramp as the canvas
//! (`snuffler_web_render::palette`) and put the same steps between panel, widget
//! and text in each mode.

use egui::{Color32, Stroke, Visuals};

/// Installs the app's light and dark visuals on the context.
///
/// Both at once, not the active one: egui keeps a `Style` per theme and a
/// light/dark switch swaps in whichever is stored, so the one not current at boot
/// has to be in place before it is ever shown.
pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(egui::Theme::Light, light());
    ctx.set_visuals_of(egui::Theme::Dark, dark());
}

/// The light chrome: ink at 11:1 on a panel one step below the canvas.
///
/// The panel sits just under the canvas white so the waveform reads as the paper
/// and the controls as the frame around it.
/// Text is the canvas trace ink, the strongest neutral the ramp has; it holds
/// 11.2:1 on the panel and 11.4:1 on a button, where egui's defaults hold 7.6 and
/// 8.8.
/// Links, warnings and errors are darkened to clear 4.5:1 on the panel, which
/// egui's `(0, 155, 255)` link and `(255, 100, 0)` warning do not.
fn light() -> Visuals {
    let panel = Color32::from_rgb(243, 244, 247);
    let ink = Color32::from_rgb(38, 42, 51);
    let text = Color32::from_rgb(48, 53, 63);
    let button = Color32::from_rgb(226, 229, 235);
    let raised = Color32::from_rgb(214, 218, 226);
    let mut visuals = Visuals::light();
    visuals.panel_fill = panel;
    visuals.window_fill = Color32::from_rgb(248, 249, 251);
    visuals.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(190, 194, 203));
    visuals.extreme_bg_color = Color32::WHITE;
    visuals.hyperlink_color = Color32::from_rgb(20, 90, 200);
    visuals.warn_fg_color = Color32::from_rgb(170, 84, 0);
    visuals.error_fg_color = Color32::from_rgb(190, 24, 24);
    visuals.selection.bg_fill = Color32::from_rgb(178, 204, 250);
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(24, 58, 120));

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.weak_bg_fill = panel;
    widgets.noninteractive.bg_fill = panel;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(200, 204, 212));
    widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, text);
    widgets.inactive.weak_bg_fill = button;
    widgets.inactive.bg_fill = button;
    widgets.inactive.fg_stroke = Stroke::new(1.0_f32, ink);
    widgets.hovered.weak_bg_fill = raised;
    widgets.hovered.bg_fill = raised;
    widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(120, 126, 138));
    widgets.hovered.fg_stroke = Stroke::new(1.5_f32, Color32::BLACK);
    widgets.active.weak_bg_fill = Color32::from_rgb(190, 195, 205);
    widgets.active.bg_fill = Color32::from_rgb(190, 195, 205);
    widgets.active.bg_stroke = Stroke::new(1.0_f32, Color32::BLACK);
    widgets.active.fg_stroke = Stroke::new(2.0_f32, Color32::BLACK);
    widgets.open.weak_bg_fill = raised;
    widgets.open.bg_fill = raised;
    widgets.open.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(160, 165, 176));
    widgets.open.fg_stroke = Stroke::new(1.0_f32, Color32::BLACK);
    visuals
}

/// The dark chrome: the canvas's own blue-black, lifted one step per layer.
///
/// egui's dark defaults are neutral greys; against the cool canvas the panel reads
/// warm and the seam between the two shows.
/// Text is lifted from egui's grey-140 (5.1:1) to 9.5:1, in step with the light
/// chrome, and the rest keeps egui's structure with the ramp's tint.
fn dark() -> Visuals {
    let panel = Color32::from_rgb(24, 26, 32);
    let text = Color32::from_rgb(186, 192, 204);
    let bright = Color32::from_rgb(214, 219, 228);
    let button = Color32::from_rgb(52, 56, 66);
    let rule = Color32::from_rgb(58, 62, 72);
    let mut visuals = Visuals::dark();
    visuals.panel_fill = panel;
    visuals.window_fill = panel;
    visuals.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(62, 66, 78));
    visuals.extreme_bg_color = Color32::from_rgb(12, 13, 17);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.weak_bg_fill = panel;
    widgets.noninteractive.bg_fill = panel;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, rule);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, text);
    widgets.inactive.weak_bg_fill = button;
    widgets.inactive.bg_fill = button;
    widgets.inactive.fg_stroke = Stroke::new(1.0_f32, bright);
    widgets.hovered.weak_bg_fill = Color32::from_rgb(64, 69, 80);
    widgets.hovered.bg_fill = Color32::from_rgb(64, 69, 80);
    widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(150, 156, 168));
    widgets.hovered.fg_stroke = Stroke::new(1.5_f32, Color32::from_rgb(240, 242, 246));
    widgets.active.weak_bg_fill = Color32::from_rgb(50, 54, 64);
    widgets.active.bg_fill = Color32::from_rgb(50, 54, 64);
    widgets.open.weak_bg_fill = Color32::from_rgb(44, 48, 58);
    widgets.open.bg_fill = panel;
    widgets.open.bg_stroke = Stroke::new(1.0_f32, rule);
    widgets.open.fg_stroke = Stroke::new(1.0_f32, bright);
    visuals
}
