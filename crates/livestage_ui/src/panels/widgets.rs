//! Small shared widgets.

use egui::{DragValue, Ui};
use livestage_core::ShowTime;

/// Seconds editor for a [`ShowTime`]. Returns true if changed.
pub fn time_edit(ui: &mut Ui, value: &mut ShowTime) -> bool {
    let mut secs = value.as_secs_f64();
    let r = ui.add(DragValue::new(&mut secs).speed(0.05).range(0.0..=36_000.0).max_decimals(2).suffix(" s"));
    if r.changed() {
        *value = ShowTime::from_secs_f64(secs);
    }
    r.changed()
}

pub fn opt_time_edit(ui: &mut Ui, value: &mut Option<ShowTime>, none_label: &str) -> bool {
    ui.horizontal(|ui| opt_time_edit_inner(ui, value, none_label)).inner
}

fn opt_time_edit_inner(ui: &mut Ui, value: &mut Option<ShowTime>, none_label: &str) -> bool {
    let mut on = value.is_some();
    let mut changed = ui.checkbox(&mut on, "").changed();
    if changed {
        *value = on.then_some(value.unwrap_or(ShowTime::from_secs(10)));
    }
    match value {
        Some(v) => changed |= time_edit(ui, v),
        None => {
            ui.weak(none_label);
        }
    }
    changed
}

pub fn fmt_dur(t: Option<ShowTime>) -> String {
    match t {
        Some(t) => t.to_clock_string()[3..].to_string(),
        None => "∞".into(),
    }
}

pub fn color_edit(ui: &mut Ui, rgba: &mut [f32; 4]) -> bool {
    let mut c = egui::Rgba::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
    let changed = egui::color_picker::color_edit_button_rgba(ui, &mut c, egui::color_picker::Alpha::OnlyBlend).changed();
    if changed {
        *rgba = c.to_rgba_unmultiplied();
    }
    changed
}
