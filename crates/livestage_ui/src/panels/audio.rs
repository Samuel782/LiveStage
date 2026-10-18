//! Audio Channel Patch Bay: device, stems, crosspoint matrix, meters.

use super::UiState;
use crate::runtime::ShowRuntime;
use crate::theme;
use egui::{Button, Color32, ComboBox, DragValue, RichText, Sense, Ui};
use livestage_audio::gain_to_db;

pub fn show(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState) {
    let mut routing_changed = false;
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        // ---- device
        ui.horizontal(|ui| {
            ui.label(RichText::new("Device").strong());
            if st.audio_devices.is_empty() || ui.small_button("Rescan").on_hover_text("Rescan devices").clicked() {
                st.audio_devices = livestage_audio::list_output_devices();
            }
            let r = &mut rt.bundle.project.audio;
            let current = r.device_name.clone().unwrap_or_else(|| "System default".into());
            ComboBox::from_id_salt("adev").width(260.0).selected_text(current).show_ui(ui, |ui| {
                if ui.selectable_label(r.device_name.is_none(), "System default").clicked() {
                    r.device_name = None;
                }
                for d in &st.audio_devices {
                    let label = format!("{} ({} ch, {} Hz)", d.name, d.max_channels, d.default_sample_rate);
                    if ui.selectable_label(r.device_name.as_deref() == Some(&d.name), label).clicked() {
                        r.device_name = Some(d.name.clone());
                    }
                }
            });
            let mut buf = r.buffer_frames.unwrap_or(256);
            ui.label("Buffer");
            if ui.add(DragValue::new(&mut buf).range(32..=4096)).changed() {
                r.buffer_frames = Some(buf);
            }
            if ui.button("Apply & restart engine").clicked() {
                rt.start_audio();
                rt.mark_changed();
            }
        });
        if let Some(e) = &rt.audio_error {
            ui.colored_label(theme::stop(), e);
        }
        let device_ch = rt.audio.as_ref().map_or(rt.bundle.project.audio.output_channels, |a| a.channels());
        ui.separator();

        // ---- meters
        if let Some(a) = &rt.audio {
            let peaks = a.take_peaks();
            st.meters.resize(peaks.len(), 0.0);
            for (m, p) in st.meters.iter_mut().zip(peaks) {
                *m = p.max(*m * 0.9); // ballistic decay
            }
        }
        let n_out = rt.bundle.project.audio.output_channels.max(device_ch) as usize;
        ui.label(RichText::new("Output meters").strong());
        ui.horizontal(|ui| {
            for ch in 0..n_out.min(64) {
                let level = st.meters.get(ch).copied().unwrap_or(0.0);
                let db = gain_to_db(level);
                let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 80.0), Sense::hover());
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 1.0, theme::PANEL_RECESSED);
                let mut bar = rect;
                bar.set_top(rect.bottom() - rect.height() * frac);
                let c = if db > -3.0 { theme::stop() } else if db > -12.0 { theme::standby() } else { theme::active() };
                p.rect_filled(bar, 1.0, c);
                let inactive = ch as u16 >= device_ch;
                if inactive {
                    p.rect_filled(rect, 1.0, Color32::from_black_alpha(160));
                }
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.bottom() + 8.0),
                    egui::Align2::CENTER_CENTER,
                    (ch + 1).to_string(),
                    egui::FontId::monospace(10.0),
                    if inactive { theme::TEXT_DIM.gamma_multiply(0.5) } else { theme::TEXT_DIM },
                );
            }
        });
        ui.add_space(14.0);
        ui.separator();

        // ---- stems
        let r = &mut rt.bundle.project.audio;
        ui.label(RichText::new("Stems").strong());
        let buses = r.buses.clone();
        let mut patch_bus = None;
        egui::Grid::new("stems").striped(true).num_columns(5).show(ui, |ui| {
            for (si, s) in r.stems.iter_mut().enumerate() {
                routing_changed |= ui.add_sized([150.0, 22.0], egui::TextEdit::singleline(&mut s.name)).changed();
                routing_changed |= ui.add(DragValue::new(&mut s.channels).range(1..=16).suffix(" ch")).changed();
                routing_changed |= ui.add(egui::Slider::new(&mut s.gain_db, -60.0..=12.0).suffix(" dB")).changed();
                routing_changed |= ui.toggle_value(&mut s.mute, "M").changed();
                ComboBox::from_id_salt(("bus", si)).selected_text("Patch to bus…").show_ui(ui, |ui| {
                    for b in &buses {
                        let outs: Vec<String> = b.channels.iter().map(|c| (c + 1).to_string()).collect();
                        if ui.button(format!("{} ({})", b.name, outs.join("-"))).clicked() {
                            patch_bus = Some((si, b.clone()));
                        }
                    }
                });
                ui.end_row();
            }
        });
        if let Some((si, bus)) = patch_bus {
            let stem = r.stems[si].clone();
            r.patch_stem_to_bus(&stem, &bus);
            routing_changed = true;
        }
        if ui.button("+ Stem").clicked() {
            r.stems.push(livestage_core::routing::Stem {
                id: Default::default(),
                name: format!("Stem {}", r.stems.len() + 1),
                channels: 2,
                gain_db: 0.0,
                mute: false,
            });
            routing_changed = true;
        }
        ui.separator();

        // ---- crosspoint matrix
        ui.horizontal(|ui| {
            ui.label(RichText::new("Patch matrix").strong());
            ui.label("Outputs");
            routing_changed |= ui.add(DragValue::new(&mut r.output_channels).range(1..=64)).changed();
        });
        let outs = r.output_channels as usize;
        if r.output_labels.len() < outs {
            for i in r.output_labels.len()..outs {
                r.output_labels.push(format!("Out {}", i + 1));
            }
        }
        let stems = r.stems.clone();
        egui::Grid::new("matrix").spacing([2.0, 2.0]).show(ui, |ui| {
            ui.label("");
            for o in 0..outs {
                let label = RichText::new((o + 1).to_string()).small();
                let label = if o as u16 >= device_ch { label.color(theme::TEXT_DIM) } else { label };
                ui.label(label).on_hover_text(r.output_label(o as u16));
            }
            ui.end_row();
            for s in &stems {
                for sc in 0..s.channels {
                    let suffix = match (s.channels, sc) {
                        (1, _) => String::new(),
                        (2, 0) => " L".into(),
                        (2, 1) => " R".into(),
                        _ => format!(" {}", sc + 1),
                    };
                    ui.label(format!("{}{suffix}", s.name));
                    for o in 0..outs as u16 {
                        let on = r.is_patched(s.id, sc, o);
                        let fill = if on { theme::active() } else { theme::PANEL_RECESSED };
                        if ui.add_sized([22.0, 20.0], Button::new("").fill(fill)).clicked() {
                            r.toggle(s.id, sc, o);
                            routing_changed = true;
                        }
                    }
                    ui.end_row();
                }
            }
        });
    });
    if routing_changed {
        rt.apply_audio_routing();
        rt.mark_changed();
    }
}
