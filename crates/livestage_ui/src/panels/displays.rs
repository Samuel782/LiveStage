//! Display Routing Manager: logical outputs → physical screens, live
//! previews, and projection mapping (Bezier warp, edge blend, masks).

use super::{NdiStatus, Previews, UiState};
use crate::runtime::ShowRuntime;
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

pub fn show(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState, physical: &[PhysicalDisplayInfo], previews: &Previews, ndi: &NdiStatus) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} physical display(s) detected", physical.len())).color(theme::TEXT_DIM));
        if ui.button("+ Output").clicked() {
            let n = rt.bundle.project.displays.len() + 1;
            rt.bundle.project.displays.push(LogicalDisplay::new(format!("Output {n}")));
            rt.mark_changed();
        }
    });
    let mut remove = None;
    let mut test_toggle = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
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
                            ui.add(egui::Image::new((*tex, egui::vec2(w, h))).bg_fill(Color32::BLACK));
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
                            dirty |= ui.checkbox(&mut d.fullscreen, "").changed();
                            ui.end_row();
                            ui.label("Enabled");
                            dirty |= ui.checkbox(&mut d.enabled, "").changed();
                            ui.end_row();
                            ui.label("NDI output");
                            ui.horizontal(|ui| {
                                let mut on = d.ndi_output.is_some();
                                if ui.checkbox(&mut on, "").changed() {
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
                                        Some(n) => ui.colored_label(if *n > 0 { theme::active() } else { theme::TEXT_DIM }, format!("{n} receiver(s)")),
                                        None if livestage_video::ndi::runtime().is_none() => ui.colored_label(theme::stop(), "NDI runtime not installed"),
                                        None => ui.weak("starting…"),
                                    };
                                }
                            });
                            ui.end_row();
                        });
                        ui.horizontal(|ui| {
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
                    dirty |= projection_editor(ui, d, st);
                });
            });
            if dirty {
                rt.mark_changed();
            }
        }
    });
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

fn projection_editor(ui: &mut Ui, d: &mut LogicalDisplay, st: &mut UiState) -> bool {
    let mut c = false;
    let id = d.id;
    let pr = &mut d.projection;
    ui.horizontal(|ui| {
        ui.label("Brightness");
        c |= ui.add(egui::Slider::new(&mut pr.brightness, 0.0..=1.5)).changed();
    });
    ui.horizontal(|ui| {
        ui.label("Warp grid");
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
    });

    // Interactive lattice editor: drag the Bezier control points.
    let w = ui.available_width().min(420.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, w * 9.0 / 16.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, theme::PANEL_RECESSED);
    let to_screen = |q: [f32; 2]| Pos2::new(rect.left() + q[0] * rect.width(), rect.top() + q[1] * rect.height());
    // Warped outline sampled from the actual surface.
    let grid = &pr.warp;
    if grid.is_valid() {
        let steps = 16;
        for k in 0..=4 {
            let t = k as f32 / 4.0;
            let row: Vec<Pos2> = (0..=steps).map(|s| to_screen(grid.evaluate(s as f32 / steps as f32, t))).collect();
            let col: Vec<Pos2> = (0..=steps).map(|s| to_screen(grid.evaluate(t, s as f32 / steps as f32))).collect();
            p.add(egui::Shape::line(row, Stroke::new(1.0_f32, theme::active())));
            p.add(egui::Shape::line(col, Stroke::new(1.0_f32, theme::active())));
        }
        for (i, q) in grid.points.iter().enumerate() {
            let hot = st.dragging_warp == Some((id, i));
            p.circle_filled(to_screen(*q), if hot { 7.0 } else { 5.0 }, if hot { theme::standby() } else { theme::TEXT });
        }
    }
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let nearest = pr
                .warp
                .points
                .iter()
                .enumerate()
                .map(|(i, q)| (i, to_screen(*q).distance(pos)))
                .filter(|(_, dist)| *dist < 14.0)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            st.dragging_warp = nearest.map(|(i, _)| (id, i));
        }
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

    ui.label(RichText::new("Edge blend").strong());
    egui::Grid::new(("blend", id)).num_columns(4).show(ui, |ui| {
        c |= edge_ui(ui, "Left", &mut pr.blend.left);
        c |= edge_ui(ui, "Right", &mut pr.blend.right);
        c |= edge_ui(ui, "Top", &mut pr.blend.top);
        c |= edge_ui(ui, "Bottom", &mut pr.blend.bottom);
    });

    ui.label(RichText::new("Masks").strong());
    let mut remove = None;
    for (i, m) in pr.masks.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            c |= ui.checkbox(&mut m.enabled, "").changed();
            c |= ui.text_edit_singleline(&mut m.name).changed();
            c |= ui.checkbox(&mut m.invert, "show inside only").changed();
            ui.weak(format!("{} pts", m.points.len()));
            if ui.small_button("✖").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        pr.masks.remove(i);
        c = true;
    }
    if ui.button("+ Mask (centre rect)").clicked() {
        pr.masks.push(PolygonMask {
            name: format!("Mask {}", pr.masks.len() + 1),
            points: vec![[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]],
            invert: false,
            enabled: true,
        });
        c = true;
    }
    // Draw masks on the lattice preview.
    for m in pr.masks.iter().filter(|m| m.enabled) {
        let pts: Vec<Pos2> = m.points.iter().map(|q| to_screen(*q)).collect();
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.5_f32, theme::stop())));
    }
    c
}
