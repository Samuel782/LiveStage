//! Audio mixer: the device bar, a vertical channel strip per stem (fader,
//! mute, solo, patch), the physical outputs' meters, the master strip, and
//! the crosspoint routing matrix in a modal.
//!
//! Solo is an audition aid: it pushes a routing to the engine with the
//! other stems muted, but never changes the show's saved mutes.

use super::widgets::inputs::fader;
use super::widgets::meter::{scale, vu_meter, Orientation};
use super::UiState;
use crate::theme;
use crate::view::UiRuntime;
use egui::{Align, Button, ComboBox, DragValue, Layout, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::id::StemId;

const STRIP_W: f32 = 96.0;
const FADER_H: f32 = 170.0;

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    let mut routing_changed = false;
    device_bar(ui, rt, st);
    ui.add_space(theme::GRID);
    let device_ch = rt.audio.as_ref().map_or(rt.bundle.project.audio.output_channels, |a| a.channels());
    egui::ScrollArea::horizontal().id_salt("mixer-scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // ---- stems
            let n = rt.bundle.project.audio.stems.len();
            for si in 0..n {
                routing_changed |= stem_strip(ui, rt, st, si);
            }
            if ui.add(Button::new("+").min_size(Vec2::new(28.0, FADER_H + 90.0))).on_hover_text("Add a stem").clicked() {
                let r = &mut rt.bundle.project.audio;
                r.stems.push(livestage_core::routing::Stem {
                    id: Default::default(),
                    name: format!("Stem {}", r.stems.len() + 1),
                    channels: 2,
                    gain_db: 0.0,
                    mute: false,
                });
                routing_changed = true;
            }
            ui.add_space(theme::GRID);
            divider(ui);
            // ---- physical outputs
            let n_out = rt.bundle.project.audio.output_channels.max(device_ch) as usize;
            if st.meters.len() < n_out {
                st.meters.resize(n_out, Default::default());
            }
            ui.vertical(|ui| {
                ui.label(super::widgets::caption_text("Outputs"));
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let (sc, _) = ui.allocate_exact_size(Vec2::new(22.0, FADER_H + 34.0), Sense::hover());
                    scale(ui.painter(), sc.top(), FADER_H + 20.0, sc.left() + 2.0);
                    for ch in 0..n_out.min(64) {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            let label = rt.bundle.project.audio.output_label(ch as u16);
                            vu_meter(ui, Vec2::new(12.0, FADER_H + 20.0), &mut st.meters[ch], Orientation::Vertical, (ch as u16) < device_ch).on_hover_text(label);
                            ui.label(RichText::new((ch + 1).to_string()).font(theme::clock_font(10.0)).color(theme::text_dim()));
                        });
                    }
                });
            });
            ui.add_space(theme::GRID);
            divider(ui);
            // ---- master
            master_strip(ui, rt, st);
        });
    });
    routing_modal(ui.ctx(), rt, st, device_ch, &mut routing_changed);
    if routing_changed {
        push_routing(rt, st);
        rt.mark_changed();
    }
}

/// Sends the routing to the engine, with solo applied when any stem is soloed.
fn push_routing(rt: &mut UiRuntime, st: &UiState) {
    if st.soloed.is_empty() {
        rt.apply_audio_routing();
    } else {
        rt.apply_audio_routing_with_solo(st.soloed.clone());
    }
}

fn divider(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, FADER_H + 90.0), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, theme::border()));
}

fn device_bar(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Device").strong());
        if st.audio_devices.is_empty() {
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
        if ui.small_button("Rescan").on_hover_text("Rescan devices").clicked() {
            st.audio_devices = livestage_audio::list_output_devices();
        }
        let mut buf = r.buffer_frames.unwrap_or(256);
        ui.label("Buffer");
        if ui.add(DragValue::new(&mut buf).range(32..=4096).suffix(" frames")).changed() {
            r.buffer_frames = Some(buf);
        }
        if ui.button("Apply & restart engine").clicked() {
            rt.start_audio();
            rt.mark_changed();
        }
        if ui.button("Routing matrix…").clicked() {
            st.routing_open = true;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &rt.audio {
            Some(a) => {
                ui.label(RichText::new(format!("{} · {} Hz · {} ch · DSP {:.0} %", a.device_name(), a.sample_rate(), a.channels(), a.dsp_load() * 100.0)).color(theme::text_dim()));
            }
            None => {
                ui.colored_label(theme::stop_text(), "Audio offline");
            }
        });
    });
    if let Some(e) = &rt.audio_error {
        ui.colored_label(theme::stop_text(), e);
    }
    if !st.soloed.is_empty() {
        ui.horizontal(|ui| {
            ui.colored_label(theme::standby(), RichText::new(format!("SOLO: {} stem(s) — the others are muted on the outputs (not saved)", st.soloed.len())).strong());
            if ui.small_button("Clear solo").clicked() {
                st.soloed.clear();
                push_routing(rt, st);
            }
        });
    }
}

/// Small square toggle (M / S) in a signal colour.
fn strip_button(ui: &mut Ui, label: &str, on: bool, color: egui::Color32, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(30.0, 22.0), Sense::click());
    let fill = if on { color } else if resp.hovered() { theme::control_hover() } else { theme::control() };
    ui.painter().rect(rect, theme::RADIUS as f32, fill, Stroke::new(theme::HAIRLINE, if on { color } else { theme::border_hi() }), StrokeKind::Inside);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), if on { theme::on(color) } else { theme::text() });
    resp.on_hover_text(tip).clicked()
}

/// One stem's strip. Returns true when the routing changed.
fn stem_strip(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState, si: usize) -> bool {
    let mut changed = false;
    let id: StemId = rt.bundle.project.audio.stems[si].id;
    let frame = egui::Frame::default().fill(theme::recessed()).stroke(theme::hairline()).corner_radius(theme::RADIUS + 2).inner_margin(egui::Margin::same(6));
    frame.show(ui, |ui| {
        ui.set_width(STRIP_W - 12.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let buses = rt.bundle.project.audio.buses.clone();
            let s = &mut rt.bundle.project.audio.stems[si];
            changed |= ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(STRIP_W - 14.0).horizontal_align(Align::Center)).changed();
            let ch = match s.channels {
                1 => "mono".to_string(),
                2 => "stereo".to_string(),
                n => format!("{n} ch"),
            };
            ui.label(RichText::new(ch).size(theme::type_scale::SMALL).color(theme::text_dim()));
            changed |= fader(ui, &mut s.gain_db, -60.0..=12.0, Vec2::new(40.0, FADER_H)).changed();
            ui.label(RichText::new(format!("{:+.1} dB", s.gain_db)).font(theme::clock_font(11.5)));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if strip_button(ui, "M", s.mute, theme::stop(), "Mute (saved with the show)") {
                    s.mute = !s.mute;
                    changed = true;
                }
                let soloed = st.soloed.contains(&id);
                if strip_button(ui, "S", soloed, theme::standby(), "Solo: audition this stem alone (not saved)") {
                    if soloed {
                        st.soloed.remove(&id);
                    } else {
                        st.soloed.insert(id);
                    }
                    changed = true;
                }
            });
            let mut patch_bus = None;
            ComboBox::from_id_salt(("bus", si)).width(STRIP_W - 14.0).selected_text("Patch…").show_ui(ui, |ui| {
                for b in &buses {
                    let outs: Vec<String> = b.channels.iter().map(|c| (c + 1).to_string()).collect();
                    if ui.button(format!("{} ({})", b.name, outs.join("-"))).clicked() {
                        patch_bus = Some(b.clone());
                    }
                }
            });
            if let Some(bus) = patch_bus {
                let r = &mut rt.bundle.project.audio;
                let stem = r.stems[si].clone();
                r.patch_stem_to_bus(&stem, &bus);
                changed = true;
            }
        });
    });
    changed
}

fn master_strip(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    ui.vertical(|ui| {
        ui.label(super::widgets::caption_text("Master"));
        ui.horizontal_top(|ui| {
            let mut db = rt.bundle.project.audio.master_gain_db;
            if fader(ui, &mut db, -60.0..=6.0, Vec2::new(40.0, FADER_H)).changed() {
                rt.command(ControlSource::Ui, ShowCommand::SetMasterGain { db });
            }
            vu_meter(ui, Vec2::new(14.0, FADER_H), &mut st.master_meter, Orientation::Vertical, rt.audio.is_some());
        });
        ui.label(RichText::new(format!("{:+.1} dB", rt.bundle.project.audio.master_gain_db)).font(theme::clock_font(11.5)));
    });
}

/// The crosspoint matrix (stems × outputs) in a window.
fn routing_modal(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState, device_ch: u16, changed: &mut bool) {
    if !st.routing_open {
        return;
    }
    let mut open = true;
    egui::Window::new("Routing matrix").open(&mut open).collapsible(false).resizable(true).default_size([720.0, 420.0]).show(ctx, |ui| {
        let r = &mut rt.bundle.project.audio;
        ui.horizontal(|ui| {
            ui.label("Physical outputs");
            *changed |= ui.add(DragValue::new(&mut r.output_channels).range(1..=64)).changed();
            ui.label(RichText::new("Click a cell to patch a stem channel to an output.").size(theme::type_scale::SMALL).color(theme::text_dim()));
        });
        let outs = r.output_channels as usize;
        if r.output_labels.len() < outs {
            for i in r.output_labels.len()..outs {
                r.output_labels.push(format!("Out {}", i + 1));
            }
        }
        let stems = r.stems.clone();
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("matrix").spacing([2.0, 2.0]).show(ui, |ui| {
                ui.label("");
                for o in 0..outs {
                    let label = RichText::new((o + 1).to_string()).font(theme::clock_font(11.0));
                    let label = if o as u16 >= device_ch { label.color(theme::text_faint()) } else { label };
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
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(22.0, 20.0), Sense::click());
                            let fill = if on { theme::active() } else if resp.hovered() { theme::control_hover() } else { theme::recessed() };
                            ui.painter().rect(rect, 3.0, fill, theme::hairline(), StrokeKind::Inside);
                            if resp.on_hover_text(format!("{}{suffix} → {}", s.name, r.output_label(o))).clicked() {
                                r.toggle(s.id, sc, o);
                                *changed = true;
                            }
                        }
                        ui.end_row();
                    }
                }
            });
        });
    });
    if !open {
        st.routing_open = false;
    }
}
