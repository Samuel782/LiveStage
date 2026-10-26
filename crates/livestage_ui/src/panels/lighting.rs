//! Lighting console tab.
//!
//! Left: the stage plot (live colour, beam direction, lasso selection,
//! drag-to-place) and the fixture tiles. Middle: encoders for the selection
//! by category (intensity, colour, position with an XY pad, beam) and the
//! category's palettes. Right: effects, and the submaster faders with
//! flash buttons and the grand master.
//!
//! Edits go to the programmer (live, highest priority); **Record cue**
//! turns it into a DMX cue that references the palettes it used.

use super::UiState;
use crate::runtime::LogLevel;
use crate::view::UiRuntime;
use crate::theme;
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use livestage_core::cue::{AnyCue, Cue, CueHeader};
use livestage_core::id::{FixtureId, SubmasterId};
use livestage_core::lighting::{Effect, EffectKind, Palette, PaletteKind};
use livestage_core::state::Rgb;
use livestage_core::ShowTime;
use std::collections::HashSet;
use std::f32::consts::PI;

/// Pointer interaction on the stage plot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StageDrag {
    Lasso(Pos2),
    Move(FixtureId),
}

fn rgb(c: [f32; 3]) -> Color32 {
    Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    toolbar(ui, rt, st);
    if rt.bundle.project.dmx.patch.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No fixtures patched").size(18.0).color(theme::TEXT_DIM));
            ui.label(RichText::new("Patch fixtures in the DMX tab, or load a demo rig to try the console.").color(theme::TEXT_DIM));
            ui.add_space(8.0);
            if ui.button(RichText::new("Patch a demo rig").strong()).on_hover_text("4 spots, 6 RGBW washes, 4 RGB floor pars, 2 blinders, with palettes, faders and effects").clicked() {
                crate::demo::lighting_rig(&mut rt.bundle.project);
                rt.rebuild_dmx();
                rt.mark_changed();
                rt.log(LogLevel::Info, "lighting: demo rig patched on universe 1");
            }
        });
        return;
    }
    // Drop selections of fixtures that were unpatched.
    let patched: Vec<FixtureId> = rt.bundle.project.dmx.patch.iter().map(|f| f.id).collect();
    rt.dmx.console.programmer.selection.retain(|f| patched.contains(f));

    let area = ui.available_rect_before_wrap();
    let wide = area.width() >= 1000.0;
    if wide {
        let w = area.width();
        let (left, rest) = area.split_left_right_at_x(area.left() + w * 0.37);
        let (mid, right) = rest.split_left_right_at_x(rest.left() + w * 0.33);
        column(ui, left.shrink2(Vec2::new(4.0, 0.0)), "lx-left", |ui| {
            let h = (ui.available_height() * 0.56).max(160.0);
            stage(ui, rt, st, h);
            ui.add_space(6.0);
            fixture_tiles(ui, rt, st);
        });
        column(ui, mid.shrink2(Vec2::new(4.0, 0.0)), "lx-mid", |ui| {
            egui::ScrollArea::vertical().id_salt("lx-mid-scroll").show(ui, |ui| {
                encoders(ui, rt, st);
                ui.add_space(8.0);
                palettes(ui, rt, st);
            });
        });
        column(ui, right.shrink2(Vec2::new(4.0, 0.0)), "lx-right", |ui| {
            let faders_h = 236.0;
            if ui.available_height() >= faders_h + 180.0 {
                // Effects scroll above faders pinned to the bottom.
                let fx_h = ui.available_height() - faders_h - 8.0;
                ui.allocate_ui(Vec2::new(ui.available_width(), fx_h), |ui| {
                    egui::ScrollArea::vertical().id_salt("lx-fx-scroll").auto_shrink([false, false]).show(ui, |ui| effects(ui, rt, st));
                });
                ui.add_space(6.0);
                submasters(ui, rt, st);
            } else {
                // Short dock: faders first (live controls), effects below, one scroll.
                egui::ScrollArea::vertical().id_salt("lx-right-scroll").show(ui, |ui| {
                    submasters(ui, rt, st);
                    ui.add_space(6.0);
                    effects(ui, rt, st);
                });
            }
        });
        ui.allocate_rect(area, Sense::hover());
    } else {
        egui::ScrollArea::vertical().id_salt("lx-narrow").auto_shrink([false, false]).show(ui, |ui| {
            stage(ui, rt, st, 260.0);
            fixture_tiles(ui, rt, st);
            ui.separator();
            encoders(ui, rt, st);
            palettes(ui, rt, st);
            ui.separator();
            effects(ui, rt, st);
            ui.separator();
            submasters(ui, rt, st);
        });
    }
}

fn column(ui: &mut Ui, rect: Rect, id: &str, f: impl FnOnce(&mut Ui)) {
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).id_salt(id).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        f(ui)
    });
}

fn section(ui: &mut Ui, title: &str) {
    super::widgets::caption(ui, title);
}

// ================================================================ toolbar ==

fn toolbar(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    let patch: Vec<(FixtureId, String)> = rt.bundle.project.dmx.patch.iter().map(|f| (f.id, f.definition.clone())).collect();
    ui.horizontal_wrapped(|ui| {
        let sel = &mut rt.dmx.console.programmer.selection;
        if ui.button("All").clicked() {
            *sel = patch.iter().map(|p| p.0).collect();
        }
        if ui.button("None").clicked() {
            sel.clear();
        }
        if ui.button("Invert").clicked() {
            *sel = patch.iter().map(|p| p.0).filter(|f| !sel.contains(f)).collect();
        }
        // Select by fixture type.
        let mut types: Vec<String> = patch.iter().map(|p| p.1.clone()).collect();
        types.dedup();
        types.sort();
        types.dedup();
        for t in types {
            let label = rt.dmx.library.get(&t).map_or(t.clone(), |d| d.model.to_string());
            if ui.small_button(label).on_hover_text("Select every fixture of this type").clicked() {
                rt.dmx.console.programmer.selection = patch.iter().filter(|p| p.1 == t).map(|p| p.0).collect();
            }
        }
        ui.separator();
        let n = rt.dmx.console.programmer.len();
        let label = RichText::new(format!("Programmer · {n}")).color(if n > 0 { theme::accent() } else { theme::TEXT_DIM });
        ui.label(label).on_hover_text("Live values (they override cues until cleared)");
        if ui.add_enabled(n > 0, egui::Button::new("Clear")).on_hover_text("Release every programmer value").clicked() {
            rt.dmx.console.programmer.clear();
        }
        if ui.add_enabled(n > 0, egui::Button::new(RichText::new("Record cue").strong())).on_hover_text("New DMX cue from the programmer (palettes by reference, running effects included)").clicked() {
            record_cue(rt, st, false);
        }
        let dmx_selected = st.selected.and_then(|id| rt.find_cue(id)).is_some_and(|c| matches!(c, AnyCue::Dmx(_)));
        if ui.add_enabled(n > 0 && dmx_selected, egui::Button::new("Update cue")).on_hover_text("Replace the selected DMX cue's look with the programmer").clicked() {
            record_cue(rt, st, true);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let dbo = rt.dmx.console.blackout;
            let b = egui::Button::new(RichText::new("DBO").strong().color(if dbo { Color32::WHITE } else { theme::TEXT })).fill(if dbo { theme::stop() } else { theme::PANEL_RAISED });
            if ui.add(b).on_hover_text("Desk blackout: intensities to zero (positions and colours hold)").clicked() {
                rt.dmx.console.blackout = !dbo;
            }
        });
    });
    ui.add_space(4.0);
}

fn record_cue(rt: &mut UiRuntime, st: &mut UiState, update: bool) {
    let (attributes, palettes) = rt.dmx.console.programmer.record();
    let effects = rt.dmx.console.console_effects();
    if update {
        let Some(id) = st.selected else { return };
        if let Some(AnyCue::Dmx(d)) = rt.find_cue_mut(id) {
            d.attributes = attributes;
            d.palettes = palettes;
            d.effects = effects;
            let n = d.header.number.clone();
            rt.mark_changed();
            rt.log(LogLevel::Info, format!("lighting: cue {n} updated from the programmer"));
        }
        return;
    }
    let number = rt.next_cue_number();
    let cue = AnyCue::Dmx(livestage_core::cue::DmxCue {
        header: CueHeader::new(number.clone(), format!("Look {number}")),
        channels: vec![],
        attributes,
        fade: ShowTime::from_secs(2),
        release_on_stop: true,
        palettes,
        effects,
    });
    let id = cue.header().id;
    if let Some(l) = rt.bundle.project.active_list_mut() {
        l.cues.push(cue);
    }
    st.selected = Some(id);
    rt.after_structure_edit();
    rt.log(LogLevel::Info, format!("lighting: recorded cue {number}"));
}

// ================================================================== stage ==

/// Stage position of each patched fixture (stored, or a default grid).
fn positions(rt: &UiRuntime) -> Vec<(FixtureId, String, [f32; 2])> {
    let patch = &rt.bundle.project.dmx.patch;
    let cols = (patch.len() as f32).sqrt().ceil().max(1.0) as usize;
    patch
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let auto = [(i % cols) as f32 / cols as f32 + 0.5 / cols as f32, 0.15 + 0.7 * (i / cols) as f32 / cols.max(1) as f32];
            (f.id, f.name.clone(), rt.bundle.project.lighting.stage.get(&f.id).copied().unwrap_or(auto))
        })
        .collect()
}

fn stage(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState, height: f32) {
    ui.horizontal(|ui| {
        section(ui, "STAGE");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.toggle_value(&mut st.light_move, "Move fixtures").on_hover_text("Drag fixtures to place them on the plot");
        });
    });
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 8.0, theme::PANEL_RECESSED);
    let deck = rect.shrink2(Vec2::new(14.0, 14.0));
    p.rect_stroke(deck, 4.0, Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Inside);
    p.text(Pos2::new(deck.center().x, deck.bottom() - 4.0), Align2::CENTER_BOTTOM, "AUDIENCE ▾", FontId::proportional(10.0), theme::TEXT_DIM);
    let to_screen = |s: [f32; 2]| Pos2::new(deck.left() + s[0] * deck.width(), deck.top() + s[1] * deck.height());
    let to_stage = |q: Pos2| [((q.x - deck.left()) / deck.width()).clamp(0.0, 1.0), ((q.y - deck.top()) / deck.height()).clamp(0.0, 1.0)];
    let r = (deck.width().min(deck.height()) * 0.035).clamp(6.0, 13.0);
    let fixtures = positions(rt);
    let selection: HashSet<FixtureId> = rt.dmx.console.programmer.selection.iter().copied().collect();

    for (id, name, at) in &fixtures {
        let c = to_screen(*at);
        let color = rt.dmx.fixture_color(*id).unwrap_or([0.0; 3]);
        // Beam direction for moving heads: pan around, tilt sets the throw.
        if let (Some(pan), Some(tilt)) = (rt.dmx.output(*id, "pan"), rt.dmx.output(*id, "tilt")) {
            let a = PI / 2.0 + (pan - 0.5) * 1.5 * PI;
            let len = r * (1.2 + 3.5 * tilt);
            let tip = c + Vec2::new(a.cos(), a.sin()) * len;
            let lit = color.iter().cloned().fold(0.0, f32::max);
            p.line_segment([c, tip], Stroke::new(3.0_f32, rgb(color).gamma_multiply(0.35 + 0.65 * lit).linear_multiply(0.8)));
        }
        p.circle_filled(c, r, Color32::from_gray(30));
        p.circle_filled(c, r * 0.82, rgb(color));
        let selected = selection.contains(id);
        p.circle_stroke(c, r + 2.0, Stroke::new(if selected { 2.5_f32 } else { 1.0_f32 }, if selected { theme::accent() } else { theme::BORDER }));
        if r >= 9.0 {
            p.text(c + Vec2::new(0.0, r + 3.0), Align2::CENTER_TOP, name, FontId::proportional(9.5), theme::TEXT_DIM);
        }
    }

    let hit = |q: Pos2| fixtures.iter().filter(|(_, _, at)| to_screen(*at).distance(q) <= r + 5.0).min_by(|a, b| to_screen(a.2).distance(q).total_cmp(&to_screen(b.2).distance(q))).map(|f| f.0);
    if resp.clicked() {
        if let Some(f) = resp.interact_pointer_pos().and_then(hit) {
            let sel = &mut rt.dmx.console.programmer.selection;
            if let Some(i) = sel.iter().position(|x| *x == f) {
                sel.remove(i);
            } else {
                sel.push(f);
            }
        }
    }
    if resp.drag_started() {
        if let Some(q) = resp.interact_pointer_pos() {
            st.light_drag = match (st.light_move, hit(q)) {
                (true, Some(f)) => Some(StageDrag::Move(f)),
                _ => Some(StageDrag::Lasso(q)),
            };
        }
    }
    match (st.light_drag, resp.interact_pointer_pos()) {
        (Some(StageDrag::Move(f)), Some(q)) if resp.dragged() => {
            rt.bundle.project.lighting.stage.insert(f, to_stage(q));
        }
        (Some(StageDrag::Lasso(start)), Some(q)) if resp.dragged() => {
            let lasso = Rect::from_two_pos(start, q);
            p.rect(lasso, 2.0, theme::accent().gamma_multiply(0.12), Stroke::new(1.0_f32, theme::accent()), egui::StrokeKind::Inside);
        }
        _ => {}
    }
    if resp.drag_stopped() {
        match st.light_drag.take() {
            Some(StageDrag::Move(_)) => rt.mark_changed(),
            Some(StageDrag::Lasso(start)) => {
                if let Some(q) = resp.interact_pointer_pos() {
                    let lasso = Rect::from_two_pos(start, q);
                    let sel = &mut rt.dmx.console.programmer.selection;
                    for (id, _, at) in &fixtures {
                        if lasso.contains(to_screen(*at)) && !sel.contains(id) {
                            sel.push(*id);
                        }
                    }
                }
            }
            None => {}
        }
    }
}

fn fixture_tiles(ui: &mut Ui, rt: &mut UiRuntime, _st: &mut UiState) {
    section(ui, "FIXTURES");
    let fixtures: Vec<(FixtureId, String, u16, u16)> = rt.bundle.project.dmx.patch.iter().map(|f| (f.id, f.name.clone(), f.universe, f.address)).collect();
    let selected_set: HashSet<FixtureId> = rt.dmx.console.programmer.selection.iter().copied().collect();
    let programmed_set: HashSet<FixtureId> = rt.dmx.console.programmer.values().map(|(f, _, _)| f).collect();
    egui::ScrollArea::vertical().id_salt("lx-tiles").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(5.0);
            for (i, (id, name, uni, addr)) in fixtures.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(96.0, 52.0), Sense::click());
                let selected = selected_set.contains(id);
                let programmed = programmed_set.contains(id);
                let p = ui.painter_at(rect);
                p.rect(rect, 6.0, if selected { theme::accent_dim() } else { theme::PANEL_RAISED }, Stroke::new(if selected { 2.0_f32 } else { 1.0_f32 }, if selected { theme::accent() } else { theme::BORDER }), egui::StrokeKind::Inside);
                let color = rt.dmx.fixture_color(*id).unwrap_or([0.0; 3]);
                p.rect_filled(Rect::from_min_size(rect.left_top() + Vec2::new(6.0, 6.0), Vec2::new(10.0, 10.0)), 2.0, rgb(color));
                p.text(rect.left_top() + Vec2::new(22.0, 4.0), Align2::LEFT_TOP, (i + 1).to_string(), FontId::monospace(11.0), theme::TEXT_DIM);
                if programmed {
                    p.text(rect.right_top() + Vec2::new(-6.0, 4.0), Align2::RIGHT_TOP, "P", FontId::monospace(11.0), theme::accent());
                }
                let g = p.layout(name.clone(), FontId::proportional(12.0), theme::TEXT, rect.width() - 12.0);
                p.galley(rect.left_top() + Vec2::new(6.0, 20.0), g, theme::TEXT);
                // Intensity bar (dimmer output).
                let level = rt.dmx.output(*id, "intensity").unwrap_or_else(|| color.iter().cloned().fold(0.0, f32::max));
                let bar = Rect::from_min_size(rect.left_bottom() + Vec2::new(6.0, -8.0), Vec2::new((rect.width() - 12.0) * level, 3.0));
                p.rect_filled(bar, 1.0, theme::standby());
                if resp.on_hover_text(format!("{name} · {}.{addr}", uni + 1)).clicked() {
                    let sel = &mut rt.dmx.console.programmer.selection;
                    if selected {
                        sel.retain(|x| x != id);
                    } else {
                        sel.push(*id);
                    }
                }
            }
        });
    });
}

// =============================================================== encoders ==

/// Attributes of the selection in one category, in fixture order.
fn selection_attributes(rt: &UiRuntime, kind: PaletteKind) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in &rt.dmx.console.programmer.selection {
        for (name, r) in rt.dmx.patch().attributes_of(*f) {
            if r.kind.category() == Some(kind) && !out.iter().any(|o| o == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Value shown for an attribute: the programmer's (if set) or the output,
/// of the first selected fixture that has it.
fn shown_value(rt: &UiRuntime, attribute: &str) -> (f32, bool) {
    let prog = &rt.dmx.console.programmer;
    let Some(f) = prog.selection.iter().find(|f| rt.dmx.patch().has_attribute(**f, attribute)) else { return (0.0, false) };
    match prog.get(*f, attribute) {
        Some(v) => (v, true),
        None => (rt.dmx.output(*f, attribute).unwrap_or(0.0), false),
    }
}

fn encoders(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    ui.horizontal(|ui| {
        for k in PaletteKind::ALL {
            if ui.selectable_label(st.light_tab == k, k.label()).clicked() {
                st.light_tab = k;
            }
        }
    });
    let n = rt.dmx.console.programmer.selection.len();
    if n == 0 {
        ui.label(RichText::new("Select fixtures on the stage plot or the tiles.").color(theme::TEXT_DIM));
        return;
    }
    let attrs = selection_attributes(rt, st.light_tab);
    ui.label(RichText::new(format!("{n} fixture(s) selected")).size(12.0).color(theme::TEXT_DIM));
    if attrs.is_empty() {
        ui.label(RichText::new(format!("The selection has no {} attributes.", st.light_tab.label().to_lowercase())).color(theme::TEXT_DIM));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if st.light_tab == PaletteKind::Position && attrs.iter().any(|a| a == "pan") && attrs.iter().any(|a| a == "tilt") {
            xy_pad(ui, rt);
        }
        for a in &attrs {
            let (v, programmed) = shown_value(rt, a);
            match super::widgets::encoder(ui, &title_case(a), v, programmed) {
                super::widgets::EncoderEvent::Set(v) => rt.dmx.programmer_set(a, v),
                super::widgets::EncoderEvent::Release => rt.dmx.programmer_release(a),
                super::widgets::EncoderEvent::None => {}
            }
        }
    });
    if st.light_tab == PaletteKind::Color && attrs.iter().any(|a| a == "red") {
        ui.horizontal(|ui| {
            ui.label("Mix");
            let mut c = [shown_value(rt, "red").0, shown_value(rt, "green").0, shown_value(rt, "blue").0];
            if egui::color_picker::color_edit_button_rgb(ui, &mut c).changed() {
                rt.dmx.programmer_set("red", c[0]);
                rt.dmx.programmer_set("green", c[1]);
                rt.dmx.programmer_set("blue", c[2]);
            }
            for (name, col) in [("Red", [1.0, 0.0, 0.0]), ("Green", [0.0, 1.0, 0.0]), ("Blue", [0.0, 0.0, 1.0]), ("White", [1.0, 1.0, 1.0])] {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                ui.painter().circle_filled(r.center(), 7.0, rgb(col));
                if resp.on_hover_text(name).clicked() {
                    rt.dmx.programmer_set("red", col[0]);
                    rt.dmx.programmer_set("green", col[1]);
                    rt.dmx.programmer_set("blue", col[2]);
                }
            }
        });
    }
}

/// Pan (x) / tilt (y) pad for the selection.
fn xy_pad(ui: &mut Ui, rt: &mut UiRuntime) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(150.0, 150.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6.0, theme::PANEL_RECESSED);
    p.line_segment([rect.center_top(), rect.center_bottom()], Stroke::new(1.0_f32, theme::BORDER));
    p.line_segment([rect.left_center(), rect.right_center()], Stroke::new(1.0_f32, theme::BORDER));
    let sel = rt.dmx.console.programmer.selection.clone();
    for f in &sel {
        if let (Some(pan), Some(tilt)) = (rt.dmx.output(*f, "pan"), rt.dmx.output(*f, "tilt")) {
            let q = Pos2::new(rect.left() + pan * rect.width(), rect.bottom() - tilt * rect.height());
            p.circle_filled(q, 4.0, theme::accent());
        }
    }
    p.text(rect.left_top() + Vec2::new(4.0, 2.0), Align2::LEFT_TOP, "PAN / TILT", FontId::proportional(9.0), theme::TEXT_DIM);
    if let Some(q) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
        let pan = ((q.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let tilt = ((rect.bottom() - q.y) / rect.height()).clamp(0.0, 1.0);
        rt.dmx.programmer_set("pan", pan);
        rt.dmx.programmer_set("tilt", tilt);
    }
}

// =============================================================== palettes ==

fn palettes(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    let kind = st.light_tab;
    ui.horizontal(|ui| {
        section(ui, &format!("{} PALETTES", kind.label().to_uppercase()));
        if ui.small_button("+ Record").on_hover_text("New palette from the programmer's values of this kind").clicked() {
            let values = rt.dmx.programmer_palette_values(kind);
            if values.is_empty() {
                rt.log(LogLevel::Warn, format!("lighting: no {} values in the programmer to record", kind.label().to_lowercase()));
            } else {
                let n = rt.bundle.project.lighting.palettes.iter().filter(|p| p.kind == kind).count() + 1;
                let mut pal = Palette::new(format!("{} {n}", kind.label()), kind, values);
                pal.swatch = swatch_of(&pal);
                rt.bundle.project.lighting.palettes.push(pal);
                rt.mark_changed();
            }
        }
    });
    let mut apply = None;
    let mut update = None;
    let mut delete = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(5.0);
        for (i, pal) in rt.bundle.project.lighting.palettes.iter().enumerate().filter(|(_, p)| p.kind == kind) {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(86.0, 40.0), Sense::click());
            let p = ui.painter_at(rect);
            let hover = resp.hovered();
            p.rect(rect, 6.0, if hover { theme::PANEL_RAISED.gamma_multiply(1.4) } else { theme::PANEL_RAISED }, Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Inside);
            if let Some(c) = pal.swatch {
                p.rect_filled(Rect::from_min_size(rect.left_top() + Vec2::new(5.0, 5.0), Vec2::new(rect.width() - 10.0, 6.0)), 2.0, theme::rgb(c));
            }
            let g = p.layout(pal.name.clone(), FontId::proportional(12.0), theme::TEXT, rect.width() - 10.0);
            p.galley(rect.left_top() + Vec2::new(6.0, 16.0), g, theme::TEXT);
            if resp.clicked() {
                apply = Some(i);
            }
            resp.context_menu(|ui| {
                if super::widgets::menu_item(ui, "Update from programmer", None).clicked() {
                    update = Some(i);
                    ui.close_menu();
                }
                if super::widgets::menu_item_danger(ui, "Delete palette", None).clicked() {
                    delete = Some(i);
                    ui.close_menu();
                }
            });
        }
    });
    if let Some(i) = apply {
        let pal = rt.bundle.project.lighting.palettes[i].clone();
        if rt.dmx.programmer_apply_palette(&pal) == 0 {
            rt.log(LogLevel::Warn, format!("lighting: palette {} has nothing for the selection", pal.name));
        }
    }
    if let Some(i) = update {
        let values = rt.dmx.programmer_palette_values(kind);
        if !values.is_empty() {
            let pal = &mut rt.bundle.project.lighting.palettes[i];
            pal.values = values;
            pal.swatch = swatch_of(pal).or(pal.swatch);
            rt.palettes_edited();
        }
    }
    if let Some(i) = delete {
        rt.bundle.project.lighting.palettes.remove(i);
        rt.palettes_edited();
    }
    ui.label(RichText::new("Click applies to the selection · cues keep palettes by reference").size(11.0).color(theme::TEXT_DIM));
}

fn swatch_of(p: &Palette) -> Option<Rgb> {
    if p.kind != PaletteKind::Color {
        return None;
    }
    let get = |a: &str| p.values.iter().find(|v| v.attribute.eq_ignore_ascii_case(a)).map_or(0.0, |v| v.value);
    let w = get("white");
    Some(Rgb(((get("red") + w).min(1.0) * 255.0) as u8, ((get("green") + w).min(1.0) * 255.0) as u8, ((get("blue") + w).min(1.0) * 255.0) as u8))
}

// ================================================================ effects ==

fn effects(ui: &mut Ui, rt: &mut UiRuntime, _st: &mut UiState) {
    let now = rt.now();
    ui.horizontal(|ui| {
        section(ui, "EFFECTS");
        let sel = rt.dmx.console.programmer.selection.clone();
        ui.add_enabled_ui(!sel.is_empty(), |ui| {
            ui.menu_button("+ FX", |ui| {
                ui.set_min_width(200.0);
                let kinds: [(&str, EffectKind); 5] = [
                    ("Pan / tilt circle", EffectKind::Circle),
                    ("Pan sine", EffectKind::Sine { attribute: "pan".into() }),
                    ("Tilt sine", EffectKind::Sine { attribute: "tilt".into() }),
                    ("Colour chase", EffectKind::ColorChase { colors: vec![Rgb(255, 0, 0), Rgb(0, 255, 0), Rgb(0, 0, 255)] }),
                    ("Dimmer chase", EffectKind::DimmerChase),
                ];
                for (label, kind) in kinds {
                    if super::widgets::menu_item(ui, label, None).clicked() {
                        let mut e = Effect::new(label, kind, sel.clone());
                        e.spread = if sel.len() > 1 { 1.0 } else { 0.0 };
                        rt.dmx.console.set_effect(e.id, true, now);
                        rt.bundle.project.lighting.effects.push(e);
                        rt.mark_changed();
                        ui.close_menu();
                    }
                }
            })
            .response
            .on_disabled_hover_text("Select fixtures first");
        });
    });
    let mut remove = None;
    let mut changed = false;
    for (i, e) in rt.bundle.project.lighting.effects.iter_mut().enumerate() {
        let running = rt.dmx.console.effect_running(e.id);
        egui::Frame::new().fill(if running { theme::accent_dim() } else { theme::PANEL_RAISED }).corner_radius(6).inner_margin(6).show(ui, |ui| {
            ui.horizontal(|ui| {
                let b = egui::Button::new(RichText::new(if running { "Stop" } else { "Run" }).strong()).min_size(Vec2::new(40.0, 22.0));
                if ui.add(b).on_hover_text(if running { "Stop" } else { "Run" }).clicked() {
                    rt.dmx.console.set_effect(e.id, !running, now);
                }
                ui.label(RichText::new(format!("{} {}", i + 1, e.name)).strong());
                ui.weak(format!("{} · {} fixtures", e.kind.label(), e.fixtures.len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✖").clicked() {
                        remove = Some(i);
                    }
                });
            });
            ui.horizontal(|ui| {
                changed |= ui.add(egui::DragValue::new(&mut e.rate_hz).range(0.01..=20.0).speed(0.01).prefix("rate ").suffix(" Hz")).changed();
                let mut size = e.size * 100.0;
                if ui.add(egui::DragValue::new(&mut size).range(0.0..=100.0).prefix("size ").suffix("%")).changed() {
                    e.size = size / 100.0;
                    changed = true;
                }
                let mut spread = e.spread * 100.0;
                if ui.add(egui::DragValue::new(&mut spread).range(0.0..=100.0).prefix("spread ").suffix("%")).changed() {
                    e.spread = spread / 100.0;
                    changed = true;
                }
            });
        });
    }
    if let Some(i) = remove {
        let id = rt.bundle.project.lighting.effects.remove(i).id;
        rt.dmx.console.set_effect(id, false, now);
        changed = true;
    }
    if changed {
        rt.mark_changed();
    }
    if rt.bundle.project.lighting.effects.is_empty() {
        ui.label(RichText::new("Select fixtures, then + FX.").color(theme::TEXT_DIM));
    }
}

// ============================================================ submasters ==

fn submasters(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    section(ui, "SUBMASTERS");
    let mut changed = false;
    egui::ScrollArea::horizontal().id_salt("lx-subs").show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let subs: Vec<(SubmasterId, String, &'static str, Option<Rgb>)> =
                rt.bundle.project.lighting.submasters.iter().map(|s| (s.id, s.name.clone(), s.kind.label(), s.color)).collect();
            for (i, (id, name, kind, color)) in subs.into_iter().enumerate() {
                let flashing = rt.dmx.console.flashes.contains(&id);
                let mut level = rt.bundle.project.lighting.submasters[i].level;
                let (moved, flash) = fader(ui, &format!("{} {name}", i + 1), kind, &mut level, color.map(theme::rgb).unwrap_or(theme::accent()), flashing, ("sub", i));
                if moved {
                    rt.bundle.project.lighting.submasters[i].level = level;
                    changed = true;
                }
                match flash {
                    Some(true) => {
                        rt.dmx.console.flashes.insert(id);
                        st.light_flash = Some(id);
                    }
                    Some(false) => {
                        rt.dmx.console.flashes.remove(&id);
                        st.light_flash = None;
                    }
                    None => {}
                }
            }
            ui.separator();
            let mut gm = rt.bundle.project.lighting.grand_master;
            let dbo = rt.dmx.console.blackout;
            let (moved, flash) = fader(ui, "Grand master", if dbo { "DBO" } else { "Master" }, &mut gm, theme::stop(), dbo, "gm");
            if moved {
                rt.bundle.project.lighting.grand_master = gm;
                changed = true;
            }
            if flash == Some(true) {
                rt.dmx.console.blackout = !dbo;
            }
        });
    });
    if changed {
        rt.mark_changed();
    }
}

/// A vertical fader with a flash / function button. Returns (level moved,
/// button pressed (Some(true)) / released (Some(false))).
fn fader(ui: &mut Ui, name: &str, kind: &str, level: &mut f32, color: Color32, lit: bool, id: impl std::hash::Hash) -> (bool, Option<bool>) {
    let mut moved = false;
    let mut flash = None;
    ui.vertical(|ui| {
        ui.set_width(66.0);
        let (top, _) = ui.allocate_exact_size(Vec2::new(66.0, 4.0), Sense::hover());
        ui.painter().rect_filled(top, 2.0, color);
        ui.add(egui::Label::new(RichText::new(name).size(11.0).strong()).truncate());
        ui.label(RichText::new(format!("{kind} · {:.0}%", *level * 100.0)).size(10.0).color(theme::TEXT_DIM));
        // Fader track.
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(66.0, 120.0), Sense::click_and_drag());
        let p = ui.painter_at(rect);
        let track = Rect::from_center_size(rect.center(), Vec2::new(8.0, rect.height() - 16.0));
        p.rect_filled(track, 4.0, theme::PANEL_RECESSED);
        let filled = Rect::from_min_max(Pos2::new(track.left(), track.bottom() - track.height() * *level), track.max);
        p.rect_filled(filled, 4.0, color.gamma_multiply(0.7));
        let y = track.bottom() - track.height() * *level;
        p.rect(Rect::from_center_size(Pos2::new(track.center().x, y), Vec2::new(40.0, 14.0)), 3.0, Color32::from_gray(200), Stroke::new(1.0_f32, Color32::from_gray(60)), egui::StrokeKind::Inside);
        p.line_segment([Pos2::new(track.center().x - 14.0, y), Pos2::new(track.center().x + 14.0, y)], Stroke::new(1.5_f32, Color32::from_gray(40)));
        if let Some(q) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
            *level = ((track.bottom() - q.y) / track.height()).clamp(0.0, 1.0);
            moved = true;
        }
        if resp.hovered() {
            let s = ui.input(|i| i.smooth_scroll_delta.y);
            if s != 0.0 {
                *level = (*level + s / 600.0).clamp(0.0, 1.0);
                moved = true;
            }
        }
        // Flash button: momentary.
        let (b, bresp) = ui.allocate_exact_size(Vec2::new(66.0, 26.0), Sense::click_and_drag());
        ui.painter().rect(b, 5.0, if lit { color } else { theme::PANEL_RAISED }, Stroke::new(1.0_f32, color.gamma_multiply(0.8)), egui::StrokeKind::Inside);
        let label = if kind == "DBO" || name == "Grand master" { "DBO" } else { "FLASH" };
        ui.painter().text(b.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), if lit { Color32::BLACK } else { theme::TEXT });
        let down = bresp.is_pointer_button_down_on();
        let key = ui.id().with(id);
        let was = ui.data(|d| d.get_temp::<bool>(key).unwrap_or(false));
        if down != was {
            ui.data_mut(|d| d.insert_temp(key, down));
            flash = Some(down);
        }
    });
    (moved, flash)
}
