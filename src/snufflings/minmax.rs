use super::Action;
use super::widgets::run_button;

pub fn panel(ui: &mut egui::Ui) -> Option<Action> {
    ui.label("Print minimum, maximum, and peak-to-peak values for the visible window.");
    run_button(ui).then_some(Action::Run("minmax"))
}
