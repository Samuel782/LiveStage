//! Display mapping: a to-scale map of the physical screens with the
//! logical outputs on them (pick an output, click a screen to assign it),
//! each output's settings and live preview, and projection mapping — the
//! Bezier warp mesh over the live picture, edge blends, and polygon masks
//! edited in place.

use super::widgets::toggle;
use super::{NdiStatus, Previews, UiState};
use crate::view::UiRuntime;
use crate::theme;
use egui::{Color32, ComboBox, DragValue, Pos2, RichText, Sense, Stroke, Ui};
use livestage_core::action::{VideoAction, VisualContent};
use livestage_core::id::CueId;
use livestage_core::projection::{BlendEdge, PolygonMask, WarpGrid};
use livestage_core::routing::{LogicalDisplay, PhysicalDisplayInfo, PhysicalDisplayKey};
use livestage_core::{OutputId, ShowTime};

/// Stable pseudo-cue id for an output's test pattern (outputs and cues
/// never share UUIDs, so reusing the output's UUID cannot collide).
fn test_cue(id: OutputId) -> CueId {
    CueId(id.0)
}

/// Mapping editor mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapMode {
    #[default]
    Warp,
    Masks,
}

/// The physical screens to scale, with the outputs mapped to each.
fn screen_layout(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState, physical: &[PhysicalDisplayInfo]) {
    ui.horizontal(|ui| {
        ui.label(super::widgets::caption_text("Screens"));
        ui.label(RichText::new(format!("{} detected", physical.len())).size(theme::type_scale::SMALL).color(theme::text_dim()));
        match st.display_assign.and_then(|id| rt.bundle.project.displays.iter().find(|d| d.id == id)) {
            Some(d) => {
                ui.label(RichText::new(format!("Click a screen for “{}” (or the preview area)", d.name)).size(theme::type_scale::SMALL).color(theme::accent()));
                if ui.small_button("Cancel").clicked() {
                    st.display_assign = None;
                }
            }
            None => {
                ui.label(RichText::new("Pick an output below, then click a screen to put it there.").size(theme::type_scale::SMALL).color(theme::text_faint()));
            }
        }
    });
    let w = ui.available_width().min(760.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 150.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect(rect, theme::RADIUS as f32 + 2.0, theme::recessed(), theme::hairline(), egui::StrokeKind::Inside);
    // Right strip: outputs without a screen (preview windows).
    let strip = egui::Rect::from_min_max(egui::pos2(rect.right() - 170.0, rect.top()), rect.max);
    let desk = egui::Rect::from_min_max(rect.min, egui::pos2(strip.left(), rect.bottom())).shrink(12.0);
    p.vline(strip.left(), rect.y_range().shrink(8.0), egui::Stroke::new(1.0, theme::border()));
    if physical.is_empty() {
        p.text(desk.center(), egui::Align2::CENTER_CENTER, "No screens detected", egui::FontId::proportional(12.0), theme::text_faint());
    }
    let (min, max) = physical.iter().fold((egui::pos2(f32::MAX, f32::MAX), egui::pos2(f32::MIN, f32::MIN)), |(lo, hi), d| {
        (lo.min(egui::pos2(d.position[0] as f32, d.position[1] as f32)), hi.max(egui::pos2((d.position[0] + d.size[0] as i32) as f32, (d.position[1] + d.size[1] as i32) as f32)))
    });
    let span = (max - min).max(egui::vec2(1.0, 1.0));
    let scale = (desk.width() / span.x).min(desk.height() / span.y);
    let origin = desk.center() - span * scale / 2.0;
    let mut assign: Option<Option<PhysicalDisplayKey>> = None;
    for (i, d) in physical.iter().enumerate() {
        let r = egui::Rect::from_min_size(origin + (egui::pos2(d.position[0] as f32, d.position[1] as f32) - min) * scale, egui::vec2(d.size[0] as f32, d.size[1] as f32) * scale).shrink(2.0);
        let resp = ui.interact(r, ui.id().with(("screen", i)), Sense::click());
        let key = PhysicalDisplayKey::from(d);
        let mapped: Vec<&str> = rt.bundle.project.displays.iter().filter(|o| o.physical.as_ref().and_then(|k| k.best_match(physical)) == Some(i)).map(|o| o.name.as_str()).collect();
        let hot = st.display_assign.is_some() && resp.hovered();
        let fill = if hot { theme::accent_dim() } else if mapped.is_empty() { theme::raised() } else { theme::wash(theme::active(), 0.18) };
        p.rect(r, theme::RADIUS as f32, fill, egui::Stroke::new(if hot { theme::FOCUS_RING } else { 1.0 }, if hot { theme::accent() } else { theme::border_hi() }), egui::StrokeKind::Inside);
        let mut y = r.top() + 6.0;
        let clip = p.with_clip_rect(r.shrink(2.0));
        clip.text(egui::pos2(r.left() + 6.0, y), egui::Align2::LEFT_TOP, format!("{}{}", d.name, if d.is_primary { " · primary" } else { "" }), egui::FontId::proportional(11.0), theme::text());
        y += 14.0;
        clip.text(egui::pos2(r.left() + 6.0, y), egui::Align2::LEFT_TOP, format!("{}×{}", d.size[0], d.size[1]), egui::FontId::monospace(10.0), theme::text_dim());
        for m in mapped {
            y += 15.0;
            clip.text(egui::pos2(r.left() + 6.0, y), egui::Align2::LEFT_TOP, format!("▣ {m}"), egui::FontId::proportional(11.0), theme::active());
        }
        if resp.clicked() && st.display_assign.is_some() {
            assign = Some(Some(key));
        }
    }
    let mut y = strip.top() + 8.0;
    p.text(egui::pos2(strip.left() + 10.0, y), egui::Align2::LEFT_TOP, "PREVIEW WINDOWS", egui::FontId::proportional(9.5), theme::text_dim());
    let strip_resp = ui.interact(strip, ui.id().with("preview-strip"), Sense::click());
    if strip_resp.clicked() && st.display_assign.is_some() {
        assign = Some(None);
    }
    for o in rt.bundle.project.displays.iter().filter(|o| o.physical.as_ref().and_then(|k| k.best_match(physical)).is_none()) {
        y += 16.0;
        p.text(egui::pos2(strip.left() + 10.0, y), egui::Align2::LEFT_TOP, format!("▢ {}", o.name), egui::FontId::proportional(11.0), theme::text());
    }
    if let (Some(target), Some(id)) = (assign, st.display_assign) {
        if let Some(d) = rt.bundle.project.displays.iter_mut().find(|d| d.id == id) {
            if let Some(k) = &target {
                if let Some(ix) = k.best_match(physical) {
                    if d.canvas_size.is_none() || d.physical.is_none() {
                        d.canvas_size = Some(physical[ix].size);
                    }
                }
            }
            d.physical = target;
            rt.mark_changed();
        }
        st.display_assign = None;
    }
}

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState, physical: &[PhysicalDisplayInfo], previews: &Previews, ndi: &NdiStatus) {
    egui::ScrollArea::vertical().id_salt("displays-scroll").auto_shrink([false, false]).show(ui, |ui| {
        screen_layout(ui, rt, st, physical);
        ui.add_space(theme::GRID);
        ui.horizontal(|ui| {
            ui.label(super::widgets::caption_text("Outputs"));
            if ui.button("+ Output").clicked() {
                let n = rt.bundle.project.displays.len() + 1;
                rt.bundle.project.displays.push(LogicalDisplay::new(format!("Output {n}")));
                rt.mark_changed();
            }
        });
        outputs(ui, rt, st, physical, previews, ndi);
    });
}

fn outputs(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState, physical: &[PhysicalDisplayInfo], previews: &Previews, ndi: &NdiStatus) {
    let mut remove = None;
    let mut test_toggle = None;
    {
        let count = rt.bundle.project.displays.len();
        for i in 0..count {
            let d = &mut rt.bundle.project.displays[i];
            let id = d.id;
            let mut dirty = false;
            egui::CollapsingHeader::new(RichText::new(&d.name).strong().size(15.0)).id_salt(id).default_open(i == 0).show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    // Live canvas preview.
                    let w = 240.0;
                    match previews.get(&id) {
                        Some((tex, (cw, ch))) => {
                            let h = w * *ch as f32 / (*cw).max(1) as f32;
                            ui.add(egui::Image::new((*tex, egui::vec2(w, h))).bg_fill(Color32::BLACK).corner_radius(theme::RADIUS));
                        }
                        None => {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(w, w * 9.0 / 16.0), Sense::hover());
                            ui.painter().rect_filled(r, 2.0, Color32::BLACK);
                        }
                    }
                    ui.vertical(|ui| {
                        egui::Grid::new(("disp", id)).num_columns(2).show(ui, |ui| {
                            ui.label("Name");
                            dirty |= ui.text_edit_singleline(&mut d.name).changed();
                            ui.end_row();
                            ui.label("Screen");
                            let current = match &d.physical {
                                Some(k) => match k.best_match(physical) {
                                    Some(ix) => format!("{} ({}×{})", physical[ix].name, physical[ix].size[0], physical[ix].size[1]),
                                    None => format!("{} (disconnected)", k.name),
                                },
                                None => "Preview window".into(),
                            };
                            ComboBox::from_id_salt(("phys", id)).width(220.0).selected_text(current).show_ui(ui, |ui| {
                                if ui.selectable_label(d.physical.is_none(), "Preview window").clicked() {
                                    d.physical = None;
                                    dirty = true;
                                }
                                for p in physical {
                                    let key = PhysicalDisplayKey::from(p);
                                    let label = format!(
                                        "{}{} — {}×{} @ {},{}",
                                        p.name,
                                        if p.is_primary { " (primary)" } else { "" },
                                        p.size[0],
                                        p.size[1],
                                        p.position[0],
                                        p.position[1]
                                    );
                                    if ui.selectable_label(d.physical.as_ref() == Some(&key), label).clicked() {
                                        if d.canvas_size.is_none() || d.physical.is_none() {
                                            d.canvas_size = Some(p.size);
                                        }
                                        d.physical = Some(key);
                                        dirty = true;
                                    }
                                }
                            });
                            ui.end_row();
                            if d.physical.as_ref().and_then(|k| k.best_match(physical)).is_some_and(|ix| physical[ix].is_primary) {
                                ui.label("");
                                ui.colored_label(theme::standby(), "Warning: primary display. Kiosk mode will cover the operator UI.");
                                ui.end_row();
                            }
                            ui.label("Canvas");
                            ui.horizontal(|ui| {
                                let mut size = d.canvas_size.unwrap_or([1920, 1080]);
                                dirty |= ui.add(DragValue::new(&mut size[0]).range(16..=8192)).changed();
                                ui.label("×");
                                dirty |= ui.add(DragValue::new(&mut size[1]).range(16..=8192)).changed();
                                d.canvas_size = Some(size);
                            });
                            ui.end_row();
                            ui.label("Kiosk fullscreen");
                            dirty |= toggle(ui, &mut d.fullscreen, "").changed();
                            ui.end_row();
                            ui.label("Enabled");
                            dirty |= toggle(ui, &mut d.enabled, "").changed();
                            ui.end_row();
                            ui.label("NDI output");
                            ui.horizontal(|ui| {
                                let mut on = d.ndi_output.is_some();
                                if toggle(ui, &mut on, "").changed() {
                                    d.ndi_output = on.then(|| format!("LiveStage {}", d.name));
                                    dirty = true;
                                }
                                if let Some(name) = &mut d.ndi_output {
                                    dirty |= ui.add(egui::TextEdit::singleline(name).desired_width(150.0)).changed();
                                    let label = d.ndi_resolution.map_or("Canvas res".to_string(), |[w, h]| format!("{w}×{h}"));
                                    ComboBox::from_id_salt(("ndi-res", id)).selected_text(label).show_ui(ui, |ui| {
                                        for (txt, v) in [("Canvas res", None), ("3840×2160", Some([3840, 2160])), ("1920×1080", Some([1920, 1080])), ("1280×720", Some([1280, 720]))] {
                                            if ui.selectable_label(d.ndi_resolution == v, txt).clicked() {
                                                d.ndi_resolution = v;
                                                dirty = true;
                                            }
                                        }
                                    });
                                    match ndi.get(&id) {
                                        Some(n) => ui.colored_label(if *n > 0 { theme::active() } else { theme::text_dim() }, format!("{n} receiver(s)")),
                                        None if livestage_video::ndi::runtime().is_none() => ui.colored_label(theme::stop(), "NDI runtime not installed"),
                                        None => ui.weak("starting…"),
                                    };
                                }
                            });
                            ui.end_row();
                        });
                        ui.horizontal(|ui| {
                            let assigning = st.display_assign == Some(id);
                            if ui.selectable_label(assigning, "Place on a screen…").on_hover_text("Then click a screen in the map above").clicked() {
                                st.display_assign = if assigning { None } else { Some(id) };
                            }
                            let on = st.test_patterns.contains(&id);
                            if ui.selectable_label(on, "Test pattern").clicked() {
                                test_toggle = Some((id, !on));
                            }
                            if ui.button("Remove output").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                });
                ui.add_space(4.0);
                egui::CollapsingHeader::new("Projection mapping").id_salt(("proj", id)).show(ui, |ui| {
                    dirty |= projection_editor(ui, d, st, previews.get(&id).copied());
                });
            });
            if dirty {
                rt.mark_changed();
            }
        }
    }
    if let Some((id, on)) = test_toggle {
        let action = if on {
            st.test_patterns.insert(id);
            VideoAction::Show {
                cue: test_cue(id),
                output: id,
                content: VisualContent::TestPattern,
                layer: 1_000,
                opacity: 1.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                sync: None,
            }
        } else {
            st.test_patterns.remove(&id);
            VideoAction::Hide { cue: test_cue(id), fade: ShowTime::ZERO }
        };
        rt.send_video(action);
    }
    if let Some(i) = remove {
        rt.bundle.project.displays.remove(i);
        rt.mark_changed();
    }
}

fn edge_ui(ui: &mut Ui, label: &str, e: &mut BlendEdge) -> bool {
    let mut c = false;
    ui.label(label);
    c |= ui.add(egui::Slider::new(&mut e.width, 0.0..=0.5).text("width")).changed();
    c |= ui.add(DragValue::new(&mut e.power).range(0.5..=6.0).speed(0.05).prefix("curve ")).changed();
    c |= ui.add(DragValue::new(&mut e.gamma).range(0.5..=3.5).speed(0.05).prefix("γ ")).changed();
    ui.end_row();
    c
}

/// Square control handle, highlighted when hot.
fn handle(p: &egui::Painter, at: Pos2, hot: bool, color: Color32) {
    let r = egui::Rect::from_center_size(at, egui::vec2(if hot { 11.0 } else { 9.0 }, if hot { 11.0 } else { 9.0 }));
    p.rect(r, 2.0, if hot { color } else { theme::bg_deep() }, Stroke::new(1.5_f32, color), egui::StrokeKind::Inside);
}

fn projection_editor(ui: &mut Ui, d: &mut LogicalDisplay, st: &mut UiState, preview: Option<(egui::TextureId, (u32, u32))>) -> bool {
    let mut c = false;
    let id = d.id;
    let pr = &mut d.projection;
    ui.horizontal(|ui| {
        super::widgets::inputs::segmented(ui, &mut st.map_mode, &[(MapMode::Warp, "Warp"), (MapMode::Masks, "Masks")]);
        ui.add_space(theme::GRID);
        ui.label("Brightness");
        c |= ui.add(egui::Slider::new(&mut pr.brightness, 0.0..=1.5)).changed();
    });
    if st.map_mode == MapMode::Warp {
        ui.horizontal(|ui| {
            ui.label("Grid");
            for n in [2usize, 3, 4, 5] {
                if ui.selectable_label(pr.warp.cols == n && pr.warp.rows == n, format!("{n}×{n}")).clicked() {
                    pr.warp = WarpGrid::identity(n, n);
                    c = true;
                }
            }
            if ui.button("Reset").clicked() {
                pr.warp = WarpGrid::identity(pr.warp.cols, pr.warp.rows);
                c = true;
            }
            ui.label(RichText::new("drag points · double-click resets one").size(theme::type_scale::SMALL).color(theme::text_dim()));
        });
    }

    // The editor surface: the live picture under the mesh / masks.
    let w = ui.available_width().min(520.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, w * 9.0 / 16.0), Sense::click_and_drag());
    let p = ui.painter_at(rect.expand(6.0));
    p.rect_filled(rect, theme::RADIUS as f32, Color32::BLACK);
    if let Some((tex, _)) = preview {
        p.image(tex, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::from_white_alpha(110));
    }
    p.rect_stroke(rect, theme::RADIUS as f32, theme::hairline(), egui::StrokeKind::Outside);
    let to_screen = |q: [f32; 2]| Pos2::new(rect.left() + q[0] * rect.width(), rect.top() + q[1] * rect.height());
    let to_unit = |q: Pos2| [(q.x - rect.left()) / rect.width(), (q.y - rect.top()) / rect.height()];
    let pointer = resp.hover_pos().or(resp.interact_pointer_pos());
    let nearest = |pts: &[[f32; 2]], at: Pos2| pts.iter().enumerate().map(|(i, q)| (i, to_screen(*q).distance(at))).filter(|(_, dist)| *dist < 14.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i);

    match st.map_mode {
        MapMode::Warp => {
            let grid = &pr.warp;
            if grid.is_valid() {
                // The warped surface (sampled) and the control net.
                let steps = 24;
                for k in 0..=4 {
                    let t = k as f32 / 4.0;
                    let row: Vec<Pos2> = (0..=steps).map(|s| to_screen(grid.evaluate(s as f32 / steps as f32, t))).collect();
                    let col: Vec<Pos2> = (0..=steps).map(|s| to_screen(grid.evaluate(t, s as f32 / steps as f32))).collect();
                    p.add(egui::Shape::line(row, Stroke::new(1.0_f32, theme::wash(theme::active(), 0.8))));
                    p.add(egui::Shape::line(col, Stroke::new(1.0_f32, theme::wash(theme::active(), 0.8))));
                }
                let net = Stroke::new(1.0_f32, theme::wash(theme::accent(), 0.55));
                for r in 0..grid.rows {
                    for col in 0..grid.cols {
                        let a = to_screen(grid.point(col, r));
                        if col + 1 < grid.cols {
                            p.line_segment([a, to_screen(grid.point(col + 1, r))], net);
                        }
                        if r + 1 < grid.rows {
                            p.line_segment([a, to_screen(grid.point(col, r + 1))], net);
                        }
                    }
                }
                let hover = pointer.and_then(|q| nearest(&grid.points, q));
                for (i, q) in grid.points.iter().enumerate() {
                    let hot = st.dragging_warp == Some((id, i)) || (st.dragging_warp.is_none() && hover == Some(i));
                    handle(&p, to_screen(*q), hot, theme::accent());
                }
            }
            if resp.double_clicked() {
                if let Some(i) = pointer.and_then(|q| nearest(&pr.warp.points, q)) {
                    let (cols, rows) = (pr.warp.cols, pr.warp.rows);
                    pr.warp.points[i] = [(i % cols) as f32 / (cols - 1) as f32, (i / cols) as f32 / (rows - 1) as f32];
                    c = true;
                }
            }
            if resp.drag_started() {
                st.dragging_warp = resp.interact_pointer_pos().and_then(|q| nearest(&pr.warp.points, q)).map(|i| (id, i));
            }
            if resp.dragged() {
                if let Some((did, i)) = st.dragging_warp {
                    if did == id {
                        let delta = resp.drag_delta();
                        let q = &mut pr.warp.points[i];
                        q[0] = (q[0] + delta.x / rect.width()).clamp(-0.5, 1.5);
                        q[1] = (q[1] + delta.y / rect.height()).clamp(-0.5, 1.5);
                        c = true;
                    }
                }
            }
            if resp.drag_stopped() {
                st.dragging_warp = None;
            }
            // Masks shown faintly for reference.
            for m in pr.masks.iter().filter(|m| m.enabled) {
                let pts: Vec<Pos2> = m.points.iter().map(|q| to_screen(*q)).collect();
                p.add(egui::Shape::closed_line(pts, Stroke::new(1.0_f32, theme::wash(theme::stop(), 0.6))));
            }
        }
        MapMode::Masks => {
            let sel = st.mask_sel.filter(|i| *i < pr.masks.len());
            for (mi, m) in pr.masks.iter().enumerate() {
                let pts: Vec<Pos2> = m.points.iter().map(|q| to_screen(*q)).collect();
                let selected = sel == Some(mi);
                let color = if !m.enabled { theme::text_faint() } else if selected { theme::stop() } else { theme::wash(theme::stop(), 0.6) };
                p.add(egui::Shape::closed_line(pts.clone(), Stroke::new(if selected { 2.0 } else { 1.0 }, color)));
                if selected {
                    let hover = pointer.and_then(|q| nearest(&m.points, q));
                    for (i, q) in pts.iter().enumerate() {
                        let hot = st.dragging_mask == Some((id, mi, i)) || (st.dragging_mask.is_none() && hover == Some(i));
                        handle(&p, *q, hot, theme::stop());
                    }
                }
            }
            if let Some(mi) = sel {
                if resp.drag_started() {
                    st.dragging_mask = resp.interact_pointer_pos().and_then(|q| nearest(&pr.masks[mi].points, q)).map(|i| (id, mi, i));
                }
                if resp.dragged() {
                    if let Some((did, m, i)) = st.dragging_mask {
                        if did == id && m == mi {
                            let delta = resp.drag_delta();
                            let q = &mut pr.masks[mi].points[i];
                            q[0] = (q[0] + delta.x / rect.width()).clamp(0.0, 1.0);
                            q[1] = (q[1] + delta.y / rect.height()).clamp(0.0, 1.0);
                            c = true;
                        }
                    }
                }
                if resp.drag_stopped() {
                    st.dragging_mask = None;
                }
                // Double-click: insert a point on the nearest edge.
                if resp.double_clicked() {
                    if let Some(q) = pointer {
                        let pts = &pr.masks[mi].points;
                        let n = pts.len();
                        let edge = (0..n)
                            .map(|i| {
                                let (a, b) = (to_screen(pts[i]), to_screen(pts[(i + 1) % n]));
                                let ab = b - a;
                                let t = ((q - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
                                (i, (a + ab * t).distance(q))
                            })
                            .min_by(|a, b| a.1.total_cmp(&b.1));
                        if let Some((i, _)) = edge {
                            let u = to_unit(q);
                            pr.masks[mi].points.insert(i + 1, [u[0].clamp(0.0, 1.0), u[1].clamp(0.0, 1.0)]);
                            c = true;
                        }
                    }
                }
                // Right-click a point: remove it (a mask keeps at least three).
                if resp.secondary_clicked() && pr.masks[mi].points.len() > 3 {
                    if let Some(i) = pointer.and_then(|q| nearest(&pr.masks[mi].points, q)) {
                        pr.masks[mi].points.remove(i);
                        c = true;
                    }
                }
            }
        }
    }

    if st.map_mode == MapMode::Masks {
        ui.label(RichText::new("Select a mask to edit it: drag points · double-click an edge to add a point · right-click a point to remove it").size(theme::type_scale::SMALL).color(theme::text_dim()));
        let mut remove = None;
        for (i, m) in pr.masks.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                if ui.selectable_label(st.mask_sel == Some(i), format!("Mask {}", i + 1)).clicked() {
                    st.mask_sel = if st.mask_sel == Some(i) { None } else { Some(i) };
                }
                c |= toggle(ui, &mut m.enabled, "").changed();
                c |= ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(140.0)).changed();
                c |= toggle(ui, &mut m.invert, "show inside only").changed();
                ui.label(RichText::new(format!("{} pts", m.points.len())).size(theme::type_scale::SMALL).color(theme::text_dim()));
                if ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            pr.masks.remove(i);
            st.mask_sel = None;
            c = true;
        }
        if ui.button("+ Mask").clicked() {
            pr.masks.push(PolygonMask {
                name: format!("Mask {}", pr.masks.len() + 1),
                points: vec![[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]],
                invert: false,
                enabled: true,
            });
            st.mask_sel = Some(pr.masks.len() - 1);
            c = true;
        }
    } else {
        ui.label(RichText::new("Edge blend").strong());
        egui::Grid::new(("blend", id)).num_columns(4).show(ui, |ui| {
            c |= edge_ui(ui, "Left", &mut pr.blend.left);
            c |= edge_ui(ui, "Right", &mut pr.blend.right);
            c |= edge_ui(ui, "Top", &mut pr.blend.top);
            c |= edge_ui(ui, "Bottom", &mut pr.blend.bottom);
        });
    }
    c
}
