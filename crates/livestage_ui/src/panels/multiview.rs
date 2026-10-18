//! Multiview: every program output as a live tile, like a gallery monitor
//! wall in the control room.

use super::{NdiStatus, Previews};
use crate::runtime::ShowRuntime;
use crate::theme;
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui};

const MIN_TILE: f32 = 300.0;
const CAPTION: f32 = 26.0;

pub fn show(ui: &mut Ui, rt: &ShowRuntime, previews: &Previews, ndi: &NdiStatus) {
    let displays: Vec<_> = rt.bundle.project.displays.iter().filter(|d| d.enabled).collect();
    if displays.is_empty() {
        ui.label(RichText::new("No enabled outputs. Add one in Display Routing.").color(theme::TEXT_DIM));
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let avail = ui.available_width();
        let gap = 10.0;
        let cols = ((avail + gap) / (MIN_TILE + gap)).floor().max(1.0) as usize;
        let tile_w = (avail - gap * (cols - 1) as f32) / cols as f32;
        let tile_h = tile_w * 9.0 / 16.0 + CAPTION;
        for row in displays.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for d in row {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(tile_w, tile_h), Sense::hover());
                    let p = ui.painter_at(rect);
                    p.rect_filled(rect, 4.0, Color32::BLACK);
                    let picture = Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.bottom() - CAPTION));
                    if let Some((tex, (cw, ch))) = previews.get(&d.id) {
                        // Contain-fit the canvas into the 16:9 tile.
                        let scale = (picture.width() / *cw as f32).min(picture.height() / *ch as f32);
                        let size = egui::vec2(*cw as f32 * scale, *ch as f32 * scale);
                        let img = Rect::from_center_size(picture.center(), size);
                        p.image(*tex, img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    let live = rt.outputs_live;
                    let border = if live { theme::stop() } else { theme::BORDER };
                    p.rect_stroke(rect, 4.0, Stroke::new(if live { 2.0_f32 } else { 1.0 }, border), egui::StrokeKind::Inside);
                    // Caption strip.
                    let cap = Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - CAPTION), rect.max);
                    p.rect_filled(cap, 0.0, theme::PANEL_RECESSED);
                    let p = ui.painter_at(cap.shrink2(egui::vec2(8.0, 0.0)));
                    p.text(cap.left_center() + egui::vec2(8.0, 0.0), Align2::LEFT_CENTER, &d.name, FontId::proportional(14.0), theme::TEXT);
                    let mut badges = Vec::new();
                    if live {
                        badges.push(("LIVE", theme::stop()));
                    }
                    if let Some(n) = ndi.get(&d.id) {
                        badges.push(("NDI", if *n > 0 { theme::go() } else { theme::TEXT_DIM }));
                    }
                    let mut x = cap.right() - 8.0;
                    for (text, color) in badges.into_iter().rev() {
                        let r = p.text(egui::pos2(x, cap.center().y), Align2::RIGHT_CENTER, text, FontId::proportional(12.0), color);
                        x = r.left() - 10.0;
                    }
                    let canvas = d.canvas_size.map_or("native".into(), |[w, h]| format!("{w}×{h}"));
                    let screen = match &d.physical {
                        Some(k) => k.name.clone(),
                        None => "preview window".into(),
                    };
                    resp.on_hover_text(format!("{}\nCanvas {canvas}\nScreen: {screen}", d.name));
                }
            });
            ui.add_space(gap);
        }
    });
}
