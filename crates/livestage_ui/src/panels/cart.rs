//! Soundboard: cue cart pages of pads for live busking.
//!
//! *Play* mode: pressing a pad (mouse, key, MIDI note, OSC) plays it per its
//! mode — Toggle, Retrigger or Hold (gate: plays while held). Pads show a
//! progress ring with the remaining time.
//!
//! *Edit* mode: click a pad to edit it (content, mode, colour, key and MIDI
//! learn); its cue also opens in the Inspector for detailed settings.

use super::widgets::{menu_item, menu_item_danger, menu_item_enabled, menu_separator};
use super::{Tab, UiState};
use crate::view::UiRuntime;
use crate::theme;
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::cart::{CartPad, PadMode, PadNote};
use livestage_core::cue::{Cue, CueKind};
use livestage_core::project::AssetKind;
use livestage_core::state::Rgb;
use livestage_core::ShowTime;

/// Grid presets offered in the toolbar (rows × columns).
const PRESETS: [(u8, u8); 5] = [(2, 4), (4, 4), (4, 6), (4, 8), (8, 8)];

/// Pad colour choices.
const PALETTE: [Rgb; 8] = [
    Rgb(0xE5, 0x39, 0x35),
    Rgb(0xFB, 0x8C, 0x00),
    Rgb(0xFD, 0xD8, 0x35),
    Rgb(0x43, 0xA0, 0x47),
    Rgb(0x00, 0xAC, 0xC1),
    Rgb(0x1E, 0x88, 0xE5),
    Rgb(0x8E, 0x24, 0xAA),
    Rgb(0xD8, 0x1B, 0x60),
];

/// Keys used by the global shortcuts; pads cannot take them.
pub const RESERVED_KEYS: [egui::Key; 10] = [
    egui::Key::Space,
    egui::Key::Escape,
    egui::Key::ArrowUp,
    egui::Key::ArrowDown,
    egui::Key::ArrowLeft,
    egui::Key::ArrowRight,
    egui::Key::PageUp,
    egui::Key::PageDown,
    egui::Key::P,
    egui::Key::R,
];

/// What the pad editor is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadLearn {
    Key { cart: usize, slot: usize },
    Midi { cart: usize, slot: usize },
}

fn reserved(key: egui::Key) -> bool {
    RESERVED_KEYS.contains(&key)
}

fn kind_color(kind: CueKind) -> Color32 {
    match kind {
        CueKind::Audio => Color32::from_rgb(0x1E, 0x88, 0xE5),
        CueKind::Video | CueKind::Slides => Color32::from_rgb(0x8E, 0x24, 0xAA),
        CueKind::Dmx => Color32::from_rgb(0x43, 0xA0, 0x47),
        CueKind::Timer => Color32::from_rgb(0xFB, 0x8C, 0x00),
        _ => Color32::from_rgb(0x60, 0x60, 0x66),
    }
}

fn pad_color(p: &CartPad) -> Color32 {
    p.color.map(theme::rgb).unwrap_or_else(|| kind_color(p.cue.kind()))
}

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    toolbar(ui, rt, st);
    if rt.bundle.project.carts.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No carts yet").size(18.0).color(theme::TEXT_DIM));
            ui.label(RichText::new("A cart is a page of pads for stingers, one-shot videos and DMX flashes.").color(theme::TEXT_DIM));
            ui.add_space(8.0);
            if ui.button(RichText::new("Create a 4 × 4 cart").strong()).clicked() {
                st.cart_page = rt.add_cart(4, 4);
                st.cart_edit = true;
            }
        });
        return;
    }
    st.cart_page = st.cart_page.min(rt.bundle.project.carts.len() - 1);
    poll_learn(ui.ctx(), rt, st);

    let avail = ui.available_rect_before_wrap();
    let editing = st.cart_edit && st.cart_slot.is_some();
    // Editor beside the grid when there is room, otherwise underneath.
    let side = avail.width() > 760.0;
    let (grid_rect, editor_rect) = match (editing, side) {
        (false, _) => (avail, None),
        (true, true) => {
            let (g, e) = avail.split_left_right_at_x(avail.right() - 300.0);
            (g.shrink2(Vec2::new(0.0, 0.0)), Some(e.shrink2(Vec2::new(8.0, 0.0))))
        }
        (true, false) => {
            let (g, e) = avail.split_top_bottom_at_y(avail.bottom() - 250.0);
            (g, Some(e.shrink2(Vec2::new(0.0, 6.0))))
        }
    };
    grid(ui, rt, st, grid_rect);
    if let Some(r) = editor_rect {
        ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
            egui::Frame::new().fill(theme::PANEL_RAISED).corner_radius(8).inner_margin(10).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("pad-editor").show(ui, |ui| editor(ui, rt, st));
            });
        });
    }
    ui.allocate_rect(avail, Sense::hover());
}

fn toolbar(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    ui.horizontal(|ui| {
        let mut select = None;
        for (i, k) in rt.bundle.project.carts.iter().enumerate() {
            let playing = k.playing();
            let label = if playing > 0 { format!("{}  ▸{playing}", k.name) } else { k.name.clone() };
            if ui.selectable_label(i == st.cart_page, label).clicked() {
                select = Some(i);
            }
        }
        if let Some(i) = select {
            st.cart_page = i;
            st.cart_slot = None;
        }
        if ui.small_button("+ Cart").on_hover_text("New 4 × 4 cart").clicked() {
            st.cart_page = rt.add_cart(4, 4);
            st.cart_slot = None;
        }
        if rt.bundle.project.carts.is_empty() {
            return;
        }
        ui.separator();
        let edit_label = if st.cart_edit { RichText::new("Done editing").strong().color(theme::accent()) } else { RichText::new("Edit pads") };
        if ui.button(edit_label).on_hover_text("Assign content, modes, colours, keys and MIDI notes").clicked() {
            st.cart_edit = !st.cart_edit;
            st.pad_learn = None;
            rt.refresh_midi_bindings();
        }
        if st.cart_edit {
            let page = st.cart_page.min(rt.bundle.project.carts.len() - 1);
            let k = &mut rt.bundle.project.carts[page];
            let mut changed = ui.add(egui::TextEdit::singleline(&mut k.name).desired_width(110.0)).changed();
            let (mut rows, mut cols) = (k.rows, k.cols);
            egui::ComboBox::from_id_salt("cart-grid").selected_text(format!("{rows} × {cols}")).width(74.0).show_ui(ui, |ui| {
                for (r, c) in PRESETS {
                    if ui.selectable_label((rows, cols) == (r, c), format!("{r} × {c}")).clicked() {
                        (rows, cols) = (r, c);
                    }
                }
            });
            ui.add(egui::DragValue::new(&mut rows).range(1..=livestage_core::cart::MAX_SIDE).prefix("rows "));
            ui.add(egui::DragValue::new(&mut cols).range(1..=livestage_core::cart::MAX_SIDE).prefix("cols "));
            if (rows, cols) != (k.rows, k.cols) {
                // Never shrink below the pads in use (nothing is deleted by resizing).
                let used = k.pads.iter().flatten().count();
                if rows as usize * cols as usize >= used {
                    k.resize(rows, cols);
                    st.cart_slot = st.cart_slot.filter(|s| *s < k.len());
                    changed = true;
                }
            }
            if changed {
                rt.mark_changed();
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let playing: usize = rt.bundle.project.carts.iter().map(|k| k.playing()).sum();
            let stop = egui::Button::new(RichText::new("Stop pads").strong().color(Color32::WHITE)).fill(if playing > 0 { theme::stop() } else { theme::PANEL_RAISED });
            if ui.add(stop).on_hover_text("Stop every playing pad (cue fade-outs)").clicked() {
                rt.command(ControlSource::Ui, ShowCommand::CartStopAll);
            }
            if st.cart_edit && ui.add(egui::Button::new(RichText::new("Delete cart").color(theme::stop())).frame(false)).clicked() {
                let page = st.cart_page;
                if rt.bundle.project.carts.get(page).is_some_and(|k| k.playing() == 0) {
                    rt.bundle.project.carts.remove(page);
                    st.cart_page = page.saturating_sub(1);
                    st.cart_slot = None;
                    rt.mark_changed();
                }
            }
        });
    });
    ui.add_space(4.0);
}

fn grid(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState, area: Rect) {
    let page = st.cart_page;
    let (rows, cols) = {
        let k = &rt.bundle.project.carts[page];
        (k.rows as usize, k.cols as usize)
    };
    const GAP: f32 = 6.0;
    let cell = Vec2::new(
        ((area.width() - GAP * (cols as f32 - 1.0)) / cols as f32).max(24.0),
        ((area.height() - GAP * (rows as f32 - 1.0)) / rows as f32).clamp(24.0, 220.0),
    );
    let primary_down = ui.input(|i| i.pointer.primary_down());
    for slot in 0..rows * cols {
        let (r, c) = (slot / cols, slot % cols);
        let rect = Rect::from_min_size(area.min + Vec2::new(c as f32 * (cell.x + GAP), r as f32 * (cell.y + GAP)), cell);
        let id = ui.id().with(("pad", page, slot));
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        let pad = rt.bundle.project.carts[page].pad(slot).cloned();

        // Play: press on pointer-down, release on pointer-up (Hold pads gate).
        if !st.cart_edit {
            let down = resp.is_pointer_button_down_on() && primary_down;
            let was = st.pad_pointer == Some((page, slot));
            if down && !was && pad.is_some() {
                rt.command(ControlSource::Ui, ShowCommand::CartPad { cart: page + 1, pad: slot + 1, down: true });
                st.pad_pointer = Some((page, slot));
            } else if !down && was {
                rt.command(ControlSource::Ui, ShowCommand::CartPad { cart: page + 1, pad: slot + 1, down: false });
                st.pad_pointer = None;
            }
        } else if resp.clicked() {
            st.cart_slot = Some(slot);
            st.pad_learn = None;
            if let Some(p) = &pad {
                st.selected = Some(p.cue.header().id);
            }
        }

        let elapsed = pad.as_ref().and_then(|p| rt.elapsed_of(p.cue.header().id));
        let selected = st.cart_edit && st.cart_slot == Some(slot);
        paint_pad(ui, rect, slot, pad.as_ref(), elapsed, selected, st.cart_edit, resp.hovered());
        pad_menu(&resp, rt, st, page, slot, pad.as_ref());
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_pad(ui: &Ui, rect: Rect, slot: usize, pad: Option<&CartPad>, elapsed: Option<ShowTime>, selected: bool, edit: bool, hovered: bool) {
    let p = ui.painter_at(rect.expand(2.0));
    let small = (rect.height() * 0.12).clamp(9.0, 13.0);
    let Some(pad) = pad else {
        let stroke = if selected { Stroke::new(2.0_f32, theme::accent()) } else { Stroke::new(1.0_f32, theme::BORDER) };
        p.rect(rect, 8.0, theme::PANEL_RECESSED, stroke, egui::StrokeKind::Inside);
        p.text(rect.left_top() + Vec2::new(8.0, 6.0), Align2::LEFT_TOP, (slot + 1).to_string(), FontId::monospace(small), theme::BORDER);
        if edit {
            p.text(rect.center(), Align2::CENTER_CENTER, "+", FontId::proportional((rect.height() * 0.3).clamp(14.0, 34.0)), theme::TEXT_DIM);
        }
        return;
    };
    let base = pad_color(pad);
    let playing = pad.is_playing();
    let fill = if playing {
        base
    } else if hovered {
        base.gamma_multiply(0.45)
    } else {
        base.gamma_multiply(0.30)
    };
    let stroke = if selected {
        Stroke::new(2.5_f32, theme::accent())
    } else if playing {
        Stroke::new(2.0_f32, Color32::WHITE)
    } else {
        Stroke::new(1.0_f32, base.gamma_multiply(0.7))
    };
    p.rect(rect, 8.0, fill, stroke, egui::StrokeKind::Inside);
    let text = if playing { Color32::BLACK } else { theme::TEXT };
    let dim = if playing { Color32::from_black_alpha(170) } else { theme::TEXT_DIM };

    // Corner labels: slot number, key / MIDI binding, mode.
    p.text(rect.left_top() + Vec2::new(8.0, 6.0), Align2::LEFT_TOP, (slot + 1).to_string(), FontId::monospace(small), dim);
    let binding = match (&pad.key, pad.midi) {
        (Some(k), Some(n)) => format!("{k} · {n}"),
        (Some(k), None) => k.clone(),
        (None, Some(n)) => n.to_string(),
        (None, None) => String::new(),
    };
    p.text(rect.right_top() + Vec2::new(-8.0, 6.0), Align2::RIGHT_TOP, binding, FontId::monospace(small), dim);
    p.text(rect.left_bottom() + Vec2::new(8.0, -6.0), Align2::LEFT_BOTTOM, format!("{} · {}", pad.mode.short(), pad.cue.kind().label()), FontId::proportional(small), dim);

    // Progress ring with the remaining time.
    let ring_r = (rect.height().min(rect.width()) * 0.26).clamp(9.0, 34.0);
    let ring_c = Pos2::new(rect.right() - ring_r - 10.0, rect.center().y + small * 0.2);
    let show_ring = rect.width() > 90.0 && rect.height() > 38.0;
    let name_right = if show_ring { ring_c.x - ring_r - 6.0 } else { rect.right() - 8.0 };
    let name_w = (name_right - rect.left() - 10.0).max(10.0);
    let name_size = (rect.height() * 0.17).clamp(10.0, 20.0);
    let galley = p.layout(pad.cue.header().name.clone(), FontId::proportional(name_size), text, name_w);
    let top = rect.center().y - galley.size().y / 2.0;
    p.galley(Pos2::new(rect.left() + 10.0, top), galley, text);

    if show_ring {
        p.circle_stroke(ring_c, ring_r, Stroke::new(3.0_f32, Color32::from_black_alpha(90)));
        let duration = pad.cue.duration().filter(|d| !d.is_zero());
        if let (Some(e), true) = (elapsed, playing) {
            let frac = duration.map_or(1.0, |d| (e.as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0));
            arc(&p, ring_c, ring_r, frac, Stroke::new(4.0_f32, text));
            let label = match duration {
                Some(d) => format!("-{}", short_time(d - e)),
                None => short_time(e),
            };
            p.text(ring_c, Align2::CENTER_CENTER, label, FontId::monospace((ring_r * 0.42).clamp(8.0, 13.0)), text);
        } else if let Some(d) = duration {
            p.text(ring_c, Align2::CENTER_CENTER, short_time(d), FontId::monospace((ring_r * 0.42).clamp(8.0, 13.0)), dim);
        }
    }
    if pad.held {
        p.rect_stroke(rect.shrink(3.0), 6.0, Stroke::new(2.0_f32, Color32::BLACK), egui::StrokeKind::Inside);
    }
}

/// Clockwise arc from 12 o'clock covering `frac` of the circle.
fn arc(p: &egui::Painter, c: Pos2, r: f32, frac: f32, stroke: Stroke) {
    if frac <= 0.0 {
        return;
    }
    let n = ((64.0 * frac).ceil() as usize).max(2);
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * frac * i as f32 / n as f32;
            c + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect();
    p.add(egui::Shape::line(pts, stroke));
}

/// `m:ss` (or `s.d` under ten seconds).
fn short_time(t: ShowTime) -> String {
    let s = t.as_secs_f64();
    if s < 10.0 {
        format!("{s:.1}")
    } else {
        let s = s.round() as u64;
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn pad_menu(resp: &egui::Response, rt: &mut UiRuntime, st: &mut UiState, page: usize, slot: usize, pad: Option<&CartPad>) {
    let list_cue = st.selected.and_then(|id| rt.bundle.project.active_list().and_then(|l| l.find(id))).cloned();
    resp.context_menu(|ui| {
        ui.set_min_width(220.0);
        if let Some(p) = pad {
            if menu_item(ui, "Edit pad…", None).clicked() {
                st.cart_edit = true;
                st.cart_slot = Some(slot);
                st.selected = Some(p.cue.header().id);
                ui.close_menu();
            }
            menu_separator(ui);
            for m in PadMode::ALL {
                let label = if p.mode == m { format!("✔ {}", m.label()) } else { format!("    {}", m.label()) };
                if menu_item(ui, &label, None).clicked() {
                    if let Some(pp) = rt.bundle.project.carts[page].pad_mut(slot) {
                        pp.mode = m;
                    }
                    rt.mark_changed();
                    ui.close_menu();
                }
            }
            menu_separator(ui);
        }
        let label = match &list_cue {
            Some(c) => format!("Use cue {} here", c.header().number),
            None => "Use selected cue here".into(),
        };
        if menu_item_enabled(ui, list_cue.is_some() && !pad.is_some_and(CartPad::is_playing), &label, None).clicked() {
            if let Some(c) = list_cue.clone() {
                rt.set_pad_cue(page, slot, c);
            }
            ui.close_menu();
        }
        if pad.is_none() {
            for kind in [CueKind::Dmx, CueKind::Video, CueKind::Timer] {
                if menu_item(ui, &format!("New {} pad", kind.label()), None).clicked() {
                    new_pad(rt, st, page, slot, kind);
                    ui.close_menu();
                }
            }
        }
        if let Some(p) = pad {
            menu_separator(ui);
            if menu_item_danger(ui, "Clear pad", None).clicked() {
                if !p.is_playing() {
                    rt.bundle.project.carts[page].pads[slot] = None;
                    rt.mark_changed();
                }
                ui.close_menu();
            }
        }
    });
}

fn new_pad(rt: &mut UiRuntime, st: &mut UiState, page: usize, slot: usize, kind: CueKind) {
    let mut cue = crate::demo::new_cue(&rt.bundle.project, kind, "");
    cue.header_mut().name = match kind {
        CueKind::Dmx => "Flash".into(),
        CueKind::Video => "One-shot".into(),
        _ => format!("{} pad", kind.label()),
    };
    if let Some(id) = rt.set_pad_cue(page, slot, cue) {
        if kind == CueKind::Dmx {
            if let Some(p) = rt.bundle.project.carts[page].pad_mut(slot) {
                // DMX flashes are momentary by default.
                p.mode = PadMode::Hold;
            }
        }
        st.cart_edit = true;
        st.cart_slot = Some(slot);
        st.selected = Some(id);
    }
}

/// Completes key / MIDI learn.
fn poll_learn(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
    match st.pad_learn {
        Some(PadLearn::Key { cart, slot }) => {
            let key = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } if modifiers.is_none() => Some(*key),
                    _ => None,
                })
            });
            if let Some(key) = key {
                if key == egui::Key::Escape {
                    st.pad_learn = None;
                } else if !reserved(key) {
                    let name = key.name().to_string();
                    // A key plays one pad per cart page.
                    let k = &mut rt.bundle.project.carts[cart];
                    for p in k.pads.iter_mut().flatten() {
                        if p.key.as_deref() == Some(&name) {
                            p.key = None;
                        }
                    }
                    if let Some(p) = k.pad_mut(slot) {
                        p.key = Some(name);
                    }
                    st.pad_learn = None;
                    rt.mark_changed();
                }
            }
        }
        Some(PadLearn::Midi { cart, slot }) => {
            let learned = rt.midi_in.as_ref().and_then(|m| m.take_learned());
            if let Some((channel, note)) = learned {
                if let Some(p) = rt.bundle.project.carts[cart].pad_mut(slot) {
                    p.midi = Some(PadNote { channel, note });
                }
                st.pad_learn = None;
                rt.mark_changed(); // also restores the live MIDI bindings
            }
        }
        None => {}
    }
}

fn editor(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    let (page, Some(slot)) = (st.cart_page, st.cart_slot) else { return };
    ui.horizontal(|ui| {
        ui.heading(format!("Pad {}", slot + 1));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✖").on_hover_text("Close the editor").clicked() {
                st.cart_slot = None;
                st.pad_learn = None;
                rt.refresh_midi_bindings();
            }
        });
    });
    let Some(pad) = rt.bundle.project.carts[page].pad(slot).cloned() else {
        ui.label(RichText::new("Empty pad").color(theme::TEXT_DIM));
        content_picker(ui, rt, st, page, slot);
        return;
    };
    let mut changed = false;
    let learn = st.pad_learn;
    {
        let p = rt.bundle.project.carts[page].pad_mut(slot).expect("pad exists");
        egui::Grid::new("pad-ed").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Name");
            changed |= ui.add(egui::TextEdit::singleline(&mut p.cue.header_mut().name).desired_width(170.0)).changed();
            ui.end_row();
            ui.label("Mode");
            ui.vertical(|ui| {
                for m in PadMode::ALL {
                    changed |= ui.radio_value(&mut p.mode, m, m.label()).changed();
                }
            });
            ui.end_row();
            ui.label("Colour");
            ui.horizontal_wrapped(|ui| {
                let (r, auto) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                ui.painter().circle_stroke(r.center(), 8.0, Stroke::new(1.5_f32, kind_color(p.cue.kind())));
                if p.color.is_none() {
                    ui.painter().circle_filled(r.center(), 3.0, theme::TEXT);
                }
                if auto.on_hover_text("By cue type").clicked() {
                    p.color = None;
                    changed = true;
                }
                for c in PALETTE {
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                    ui.painter().circle_filled(r.center(), 8.0, theme::rgb(c));
                    if p.color == Some(c) {
                        ui.painter().circle_stroke(r.center(), 10.0, Stroke::new(1.5_f32, theme::TEXT));
                    }
                    if resp.clicked() {
                        p.color = Some(c);
                        changed = true;
                    }
                }
            });
            ui.end_row();
            ui.label("Key");
            ui.horizontal(|ui| {
                let listening = learn == Some(PadLearn::Key { cart: page, slot });
                let text = if listening { "Press a key…".to_string() } else { p.key.clone().unwrap_or_else(|| "—".into()) };
                ui.label(RichText::new(text).font(FontId::monospace(14.0)).color(if listening { theme::standby() } else { theme::TEXT }));
                if ui.small_button(if listening { "Cancel" } else { "Set" }).clicked() {
                    st.pad_learn = if listening { None } else { Some(PadLearn::Key { cart: page, slot }) };
                }
                if p.key.is_some() && ui.small_button("Clear").clicked() {
                    p.key = None;
                    changed = true;
                }
            });
            ui.end_row();
            ui.label("MIDI");
            ui.horizontal(|ui| {
                let listening = learn == Some(PadLearn::Midi { cart: page, slot });
                let text = if listening { "Play a note…".to_string() } else { p.midi.map_or("—".into(), |n| n.to_string()) };
                ui.label(RichText::new(text).font(FontId::monospace(14.0)).color(if listening { theme::standby() } else { theme::TEXT }));
                if ui.small_button(if listening { "Cancel" } else { "Learn" }).clicked() {
                    st.pad_learn = if listening { None } else { Some(PadLearn::Midi { cart: page, slot }) };
                }
                if p.midi.is_some() && ui.small_button("Clear").clicked() {
                    p.midi = None;
                    changed = true;
                }
            });
            ui.end_row();
            if let Some(n) = &mut p.midi {
                ui.label("");
                ui.horizontal(|ui| {
                    let mut ch = n.channel + 1;
                    changed |= ui.add(egui::DragValue::new(&mut ch).range(1..=16).prefix("ch ")).changed();
                    n.channel = ch - 1;
                    changed |= ui.add(egui::DragValue::new(&mut n.note).range(0..=127).prefix("note ")).changed();
                });
                ui.end_row();
            }
        });
    }
    match st.pad_learn {
        Some(PadLearn::Midi { .. }) if learn != st.pad_learn => {
            // Silence bindings while learning so the note does not fire GO / pads.
            if let Some(m) = &rt.midi_in {
                m.take_learned();
            }
            rt.silence_midi_bindings();
        }
        _ if learn != st.pad_learn => rt.refresh_midi_bindings(),
        _ => {}
    }
    if matches!(st.pad_learn, Some(PadLearn::Midi { .. })) && rt.midi_in.is_none() {
        ui.colored_label(theme::standby(), "No MIDI input open (Settings → Control).");
    }
    if let Some(n) = pad.midi {
        let default_hit = livestage_control::midi::default_bindings().iter().any(|b| {
            matches!(b.trigger, livestage_control::midi::MidiTrigger::Note { channel, note } if channel == n.channel && note == n.note)
        });
        if default_hit {
            ui.colored_label(theme::standby(), "This note is also a default GO / STOP / PANIC note; the pad takes it.");
        }
    }
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} cue", pad.cue.kind().label())).strong());
        if ui.link("Open in Inspector").clicked() {
            st.selected = Some(pad.cue.header().id);
            st.focus_tab(Tab::Inspector);
        }
    });
    hint(ui, "Fades, gain, output, looks and the rest are set in the Inspector.");
    content_picker(ui, rt, st, page, slot);
    ui.add_space(6.0);
    if ui.add_enabled(!pad.is_playing(), egui::Button::new(RichText::new("Clear pad").color(theme::stop()))).clicked() {
        rt.bundle.project.carts[page].pads[slot] = None;
        st.selected = None;
        changed = true;
    }
    if changed {
        rt.mark_changed();
    }
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(theme::TEXT_DIM));
}

/// Ways to put content on a pad: a media asset, the selected list cue, or a new cue.
fn content_picker(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState, page: usize, slot: usize) {
    let playing = rt.bundle.project.carts[page].pad(slot).is_some_and(CartPad::is_playing);
    ui.add_enabled_ui(!playing, |ui| {
        let media: Vec<_> =
            rt.bundle.project.assets.iter().filter(|a| matches!(a.kind, AssetKind::Audio | AssetKind::Video | AssetKind::Image)).cloned().collect();
        ui.horizontal(|ui| {
            ui.label("Media");
            egui::ComboBox::from_id_salt(("pad-asset", page, slot)).width(180.0).selected_text("Choose a file…").show_ui(ui, |ui| {
                for a in &media {
                    let icon = match a.kind {
                        AssetKind::Audio => "♪",
                        _ => "▣",
                    };
                    if ui.selectable_label(false, format!("{icon} {}", a.original_name)).clicked() {
                        let name = a.original_name.rsplit_once('.').map_or(a.original_name.clone(), |(n, _)| n.to_string());
                        if let Some(cue) = rt.cue_for_asset(a, "", &name) {
                            let cue = one_shot(cue);
                            st.selected = rt.set_pad_cue(page, slot, cue);
                        }
                    }
                }
            });
        });
        let list_cue = st.selected.and_then(|id| rt.bundle.project.active_list().and_then(|l| l.find(id))).cloned();
        ui.horizontal(|ui| {
            if let Some(c) = list_cue {
                if ui.button(format!("Use cue {} {}", c.header().number, c.header().name)).clicked() {
                    st.selected = rt.set_pad_cue(page, slot, c);
                }
            }
            if ui.small_button("+ DMX flash").clicked() {
                new_pad(rt, st, page, slot, CueKind::Dmx);
            }
        });
    });
}

/// Pads play media once: no click track or loop by default.
fn one_shot(mut cue: livestage_core::AnyCue) -> livestage_core::AnyCue {
    if let livestage_core::AnyCue::Audio(a) = &mut cue {
        a.click = None;
        a.looped = false;
    }
    cue
}

/// Pad keyboard triggers. Keys of the visible cart win over other carts.
/// Called by the global shortcut handler before the fixed shortcuts.
pub fn key_triggers(ctx: &egui::Context, rt: &UiRuntime, st: &UiState) {
    let carts = &rt.bundle.project.carts;
    if carts.is_empty() || st.pad_learn.is_some() {
        return;
    }
    let order = std::iter::once(st.cart_page).chain((0..carts.len()).filter(|&i| i != st.cart_page));
    let mut map: Vec<(egui::Key, usize, usize)> = Vec::new();
    for ci in order.filter(|&i| i < carts.len()) {
        for (slot, p) in carts[ci].pads.iter().enumerate() {
            let Some(key) = p.as_ref().and_then(|p| p.key.as_deref()).and_then(egui::Key::from_name) else { continue };
            if !map.iter().any(|(k, ..)| *k == key) {
                map.push((key, ci, slot));
            }
        }
    }
    if map.is_empty() {
        return;
    }
    ctx.input_mut(|i| {
        let mut consumed = Vec::new();
        for e in &i.events {
            let egui::Event::Key { key, pressed, repeat, modifiers, .. } = e else { continue };
            if !modifiers.is_none() || *repeat {
                continue;
            }
            if let Some(&(k, ci, slot)) = map.iter().find(|(k, ..)| k == key) {
                rt.command(ControlSource::Keyboard, ShowCommand::CartPad { cart: ci + 1, pad: slot + 1, down: *pressed });
                consumed.push(k);
            }
        }
        for k in consumed {
            i.consume_key(egui::Modifiers::NONE, k);
        }
    });
}
