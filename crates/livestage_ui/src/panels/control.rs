//! Control surfaces status and the event log.

use super::UiState;
use crate::runtime::LogLevel;
use crate::view::UiRuntime;
use crate::theme;
use egui::{ComboBox, RichText, Ui};

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    egui::Grid::new("ctl").num_columns(2).show(ui, |ui| {
        ui.label("OSC (UDP)");
        ui.label(rt.osc_port().map_or("disabled".into(), |p| format!("listening on :{p}  —  /livestage/go, /livestage/cue/{{n}}/go, /livestage/panic")));
        ui.end_row();
        ui.label("WebSocket");
        ui.label(rt.ws_port().map_or("disabled".into(), |p| format!("ws://<host>:{p}  —  send {{\"cmd\":\"go\"}}")));
        ui.end_row();
        ui.label("MIDI in");
        ui.horizontal(|ui| {
            if st.midi_inputs.is_empty() || ui.small_button("Rescan").clicked() {
                st.midi_inputs = livestage_control::midi::list_inputs();
            }
            let current = rt.midi_in.as_ref().map_or("— none —".to_string(), |m| m.port_name.clone());
            let mut pick = None;
            ComboBox::from_id_salt("midi").selected_text(current).show_ui(ui, |ui| {
                for p in &st.midi_inputs {
                    if ui.selectable_label(false, p).clicked() {
                        pick = Some(p.clone());
                    }
                }
            });
            if let Some(p) = pick {
                rt.bundle.project.control.midi_input = Some(p.clone());
                rt.open_midi(Some(&p));
                rt.mark_changed();
            }
            ui.weak("Note 60/61/62 = GO/STOP/PANIC · MSC GO/STOP/RESUME/ALL_OFF");
        });
        ui.end_row();
        ui.label("Keyboard");
        ui.weak("Space GO · Esc PANIC (×2 = hard) · Up/Down standby · P pause · R resume · Cmd+S save · Cmd+Z undo");
        ui.end_row();
    });
    ui.separator();
    super::widgets::caption(ui, "Event log");
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        for l in &rt.log {
            let c = match l.level {
                LogLevel::Info => theme::TEXT_DIM,
                LogLevel::Warn => theme::standby(),
                LogLevel::Error => theme::stop(),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(l.at.to_clock_string()).monospace().color(theme::TEXT_DIM));
                ui.colored_label(c, &l.text);
            });
        }
    });
}
