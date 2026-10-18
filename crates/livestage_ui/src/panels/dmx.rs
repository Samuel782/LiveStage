//! DMX: universes, fixture patch, live universe view.

use super::UiState;
use crate::runtime::ShowRuntime;
use crate::theme;
use egui::{Align2, Color32, ComboBox, DragValue, FontId, RichText, Sense, Ui};
use livestage_core::dmx_config::{DmxTransport, PatchedFixture};

pub fn show(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState) {
    let mut apply = false;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Universes").strong());
            ui.weak(format!("{} packets sent", rt.dmx.packets_sent()));
        });
        let cfg = &mut rt.bundle.project.dmx;
        egui::Grid::new("unis").striped(true).num_columns(4).show(ui, |ui| {
            for u in &mut cfg.universes {
                ui.label(format!("{}", u.index + 1));
                ui.add_sized([140.0, 22.0], egui::TextEdit::singleline(&mut u.name));
                let kind = match &u.transport {
                    DmxTransport::ArtNet { .. } => "Art-Net",
                    DmxTransport::Sacn { .. } => "sACN",
                    DmxTransport::Disabled => "Off",
                };
                ComboBox::from_id_salt(("tr", u.index)).selected_text(kind).show_ui(ui, |ui| {
                    if ui.selectable_label(kind == "sACN", "sACN").clicked() {
                        u.transport = DmxTransport::Sacn { universe: u.index + 1, priority: 100, destination: None };
                    }
                    if ui.selectable_label(kind == "Art-Net", "Art-Net").clicked() {
                        u.transport = DmxTransport::ArtNet { port_address: u.index, destination: "2.255.255.255".into() };
                    }
                    if ui.selectable_label(kind == "Off", "Off").clicked() {
                        u.transport = DmxTransport::Disabled;
                    }
                });
                match &mut u.transport {
                    DmxTransport::ArtNet { port_address, destination } => {
                        ui.horizontal(|ui| {
                            ui.add(DragValue::new(port_address).range(0..=32767).prefix("port-addr "));
                            ui.add(egui::TextEdit::singleline(destination).desired_width(130.0));
                        });
                    }
                    DmxTransport::Sacn { universe, priority, destination } => {
                        ui.horizontal(|ui| {
                            ui.add(DragValue::new(universe).range(1..=63999).prefix("univ "));
                            ui.add(DragValue::new(priority).range(0..=200).prefix("prio "));
                            let mut d = destination.clone().unwrap_or_default();
                            if ui.add(egui::TextEdit::singleline(&mut d).hint_text("multicast").desired_width(120.0)).changed() {
                                *destination = (!d.trim().is_empty()).then(|| d.trim().to_string());
                            }
                        });
                    }
                    DmxTransport::Disabled => {
                        ui.label("");
                    }
                }
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            if ui.button("+ Universe").clicked() {
                let index = cfg.universes.iter().map(|u| u.index + 1).max().unwrap_or(0);
                cfg.universes.push(livestage_core::dmx_config::UniverseConfig {
                    index,
                    name: format!("Universe {}", index + 1),
                    transport: DmxTransport::Sacn { universe: index + 1, priority: 100, destination: None },
                });
            }
            if ui.button("Apply DMX configuration").clicked() {
                apply = true;
            }
        });
        ui.separator();

        ui.label(RichText::new("Fixture patch").strong());
        let defs: Vec<(String, String, Vec<String>)> = rt
            .dmx
            .library
            .iter()
            .map(|d| (d.key.to_string(), format!("{} {}", d.manufacturer, d.model), d.modes.iter().map(|m| m.name.to_string()).collect()))
            .collect();
        let cfg = &mut rt.bundle.project.dmx;
        let mut remove = None;
        egui::Grid::new("patch").striped(true).num_columns(5).show(ui, |ui| {
            ui.weak("Name");
            ui.weak("Type");
            ui.weak("Mode");
            ui.weak("Univ.Addr");
            ui.end_row();
            for (i, f) in cfg.patch.iter_mut().enumerate() {
                ui.add_sized([160.0, 22.0], egui::TextEdit::singleline(&mut f.name));
                ui.label(defs.iter().find(|d| d.0 == f.definition).map_or(f.definition.clone(), |d| d.1.clone()));
                ui.label(&f.mode);
                ui.horizontal(|ui| {
                    let mut u1 = f.universe + 1;
                    if ui.add(DragValue::new(&mut u1).range(1..=512)).changed() {
                        f.universe = u1 - 1;
                    }
                    ui.label(".");
                    ui.add(DragValue::new(&mut f.address).range(1..=512));
                });
                if ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            cfg.patch.remove(i);
        }
        ui.horizontal(|ui| {
            let label = defs.iter().find(|d| d.0 == st.new_fixture_def).map_or("?".into(), |d| d.1.clone());
            ComboBox::from_id_salt("newfx").selected_text(label).show_ui(ui, |ui| {
                for d in &defs {
                    ui.selectable_value(&mut st.new_fixture_def, d.0.clone(), &d.1);
                }
            });
            if ui.button("+ Patch fixture").clicked() {
                if let Some(def) = rt.dmx.library.get(&st.new_fixture_def) {
                    let mode = &def.modes[0];
                    let next = cfg.patch.iter().filter(|f| f.universe == 0).map(|f| {
                        f.address + rt.dmx.library.get(&f.definition).and_then(|d| d.mode(&f.mode)).map_or(1, |m| m.footprint)
                    });
                    let address = next.max().unwrap_or(1);
                    cfg.patch.push(PatchedFixture {
                        id: Default::default(),
                        name: format!("{} {}", def.model, cfg.patch.len() + 1),
                        definition: def.key.to_string(),
                        mode: mode.name.to_string(),
                        universe: 0,
                        address,
                    });
                }
            }
        });
        for e in &rt.dmx.patch_errors {
            ui.colored_label(theme::stop(), format!("Problem: {e}"));
        }
        ui.separator();

        // Universe 1 live view.
        ui.label(RichText::new("Universe 1 output").strong());
        if let Some(u) = rt.dmx.universe(0) {
            let cols = 32;
            let cell = ((ui.available_width() - 4.0) / cols as f32).floor().clamp(14.0, 28.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(cell * cols as f32, cell * 16.0), Sense::hover());
            let p = ui.painter_at(rect);
            for i in 0..512usize {
                let (x, y) = ((i % cols) as f32, (i / cols) as f32);
                let r = egui::Rect::from_min_size(rect.min + egui::vec2(x * cell, y * cell), egui::vec2(cell - 1.0, cell - 1.0));
                let v = u.data[i];
                let fill = if v == 0 { egui::Color32::from_rgb(0x2A, 0x2A, 0x2A) } else { theme::active().gamma_multiply(0.25 + 0.75 * v as f32 / 255.0) };
                p.rect_filled(r, 1.0, fill);
                if cell >= 20.0 && v > 0 {
                    p.text(r.center(), Align2::CENTER_CENTER, v.to_string(), FontId::monospace(8.0), Color32::BLACK);
                }
            }
        }
    });
    if apply {
        let cfg = rt.bundle.project.dmx.clone();
        rt.dmx = livestage_dmx::DmxEngine::new(cfg);
        rt.dmx.start_output();
        rt.mark_changed();
    }
}
