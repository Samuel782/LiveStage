//! Timeline editor: stacked cues on a time axis, drag to re-time.

use super::UiState;
use crate::view::UiRuntime;
use crate::theme;
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui};
use livestage_core::cue::{AnyCue, Cue, CueExt, CueKind};
use livestage_core::id::CueId;
use livestage_core::ShowTime;

const GUTTER: f32 = 190.0;
const ROW: f32 = 30.0;
const RULER: f32 = 24.0;

fn kind_color(k: CueKind) -> Color32 {
    match k {
        CueKind::Audio => Color32::from_rgb(0x3D, 0x7E, 0xC9),
        CueKind::Video => Color32::from_rgb(0x8E, 0x5C, 0xC7),
        CueKind::Timer => theme::standby(),
        CueKind::Dmx => theme::active(),
        CueKind::Control => Color32::from_rgb(0x78, 0x78, 0x78),
        CueKind::Slides => Color32::from_rgb(0xC0, 0x6A, 0x2B),
        CueKind::Timeline => Color32::from_rgb(0x2E, 0x9C, 0x9C),
    }
}

/// The timeline to show: the selected one, the one containing the selected
/// child, or the first timeline in the active list.
fn target(rt: &UiRuntime, st: &UiState) -> Option<CueId> {
    let list = rt.bundle.project.active_list()?;
    let timelines = || list.cues.iter().filter(|c| matches!(c, AnyCue::Timeline(_)));
    st.selected
        .and_then(|sel| timelines().find(|c| c.find(sel).is_some()))
        .or_else(|| timelines().next())
        .map(|c| c.header().id)
}

/// Picks a ruler step so labels are at least ~70 px apart.
fn ruler_step(zoom: f32) -> f32 {
    [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0]
        .into_iter()
        .find(|s| s * zoom >= 70.0)
        .unwrap_or(600.0)
}

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    let Some(tid) = target(rt, st) else {
        ui.label(RichText::new("No timeline cue in this list. Add one with “+ Add cue → Timeline”.").color(theme::TEXT_DIM));
        return;
    };
    let playhead = rt.elapsed_of(tid);
    let mut changed = false;
    {
        let Some(AnyCue::Timeline(t)) = rt.find_cue_mut(tid) else { return };
        let beat = t.master.as_deref().and_then(|m| match m {
            AnyCue::Audio(a) => a.click.as_ref().map(|c| 60.0 / c.bpm as f32),
            _ => None,
        });
        let content_end = t.items.iter().map(|i| i.end().unwrap_or(i.offset + ShowTime::from_secs(5))).max().unwrap_or_default();
        let length = t.duration().unwrap_or(content_end).max(content_end).max(ShowTime::from_secs(10)).as_secs_f32();

        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  {}", t.header.number, t.header.name)).strong().size(16.0));
            ui.weak(format!("{:.1} s", length));
            ui.separator();
            ui.add(egui::Slider::new(&mut st.tl_zoom, 4.0..=400.0).logarithmic(true).text("px/s"));
            let snap_label = match beat {
                Some(b) => format!("Snap to beat ({:.0} bpm)", 60.0 / b),
                None => "Snap 0.1 s".to_string(),
            };
            ui.checkbox(&mut st.tl_snap, snap_label);
        });

        let zoom = st.tl_zoom;
        let rows = 1 + t.items.len();
        let width = GUTTER + length * zoom + 120.0;
        let height = RULER + rows as f32 * ROW + 8.0;
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            let (area, _) = ui.allocate_exact_size(egui::vec2(width.max(ui.available_width()), height), Sense::hover());
            let p = ui.painter_at(area);
            let x_of = |secs: f32| area.left() + GUTTER + secs * zoom;
            p.rect_filled(area, 0.0, theme::PANEL_RECESSED);

            // Ruler + grid.
            let step = ruler_step(zoom);
            let mut s = 0.0f32;
            while s <= length + step {
                let x = x_of(s);
                p.line_segment([Pos2::new(x, area.top() + RULER - 6.0), Pos2::new(x, area.bottom())], Stroke::new(1.0_f32, theme::BORDER));
                let label = ShowTime::from_secs_f64(s as f64).to_clock_string();
                p.text(Pos2::new(x + 3.0, area.top() + 4.0), Align2::LEFT_TOP, &label[3..10], FontId::monospace(11.0), theme::TEXT_DIM);
                s += step;
            }

            let row_rect = |i: usize| {
                let y = area.top() + RULER + i as f32 * ROW;
                Rect::from_min_max(Pos2::new(area.left(), y), Pos2::new(area.right(), y + ROW))
            };
            let block = |i: usize, start: f32, dur: Option<f32>| {
                let r = row_rect(i);
                let w = dur.map_or(14.0, |d| (d * zoom).max(10.0));
                Rect::from_min_size(Pos2::new(x_of(start), r.top() + 4.0), egui::vec2(w, ROW - 8.0))
            };

            // Master lane.
            let r0 = row_rect(0);
            p.text(Pos2::new(r0.left() + 8.0, r0.center().y), Align2::LEFT_CENTER, "MASTER CLOCK", FontId::proportional(11.0), theme::TEXT_DIM);
            match t.master.as_deref() {
                Some(m) => {
                    let b = block(0, 0.0, Some(m.duration().map_or(length, |d| d.as_secs_f32())));
                    p.rect_filled(b, 3.0, kind_color(m.kind()).gamma_multiply(0.7));
                    p.with_clip_rect(b.shrink(2.0)).text(
                        b.left_center() + egui::vec2(6.0, 0.0),
                        Align2::LEFT_CENTER,
                        &m.header().name,
                        FontId::proportional(13.0),
                        Color32::WHITE,
                    );
                    let resp = ui.interact(b, ui.id().with(("tl-master", m.header().id)), Sense::click());
                    if resp.clicked() {
                        st.selected = Some(m.header().id);
                    }
                }
                None => {
                    p.text(Pos2::new(x_of(0.0) + 6.0, r0.center().y), Align2::LEFT_CENTER, "show clock (no master)", FontId::proportional(12.0), theme::TEXT_DIM);
                }
            }

            // Item lanes.
            let mut drag_ended = false;
            for (i, item) in t.items.iter_mut().enumerate() {
                let lane = i + 1;
                let r = row_rect(lane);
                let id = item.cue.header().id;
                let selected = st.selected == Some(id);
                if selected {
                    p.rect_filled(r, 0.0, Color32::from_rgb(0x22, 0x2A, 0x34));
                }
                let label = format!("{}  {}", item.cue.header().number, item.cue.header().name);
                let gutter = Rect::from_min_max(r.min, Pos2::new(r.left() + GUTTER - 10.0, r.bottom()));
                p.with_clip_rect(gutter).text(Pos2::new(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.0), theme::TEXT);
                let dur = item.cue.duration().map(|d| d.as_secs_f32());
                let b = block(lane, item.offset.as_secs_f32(), dur);
                let color = kind_color(item.cue.kind());
                let fired = item.cue.state().is_active();
                p.rect_filled(b, 3.0, if fired { color } else { color.gamma_multiply(0.6) });
                if selected {
                    p.rect_stroke(b, 3.0, Stroke::new(1.5_f32, Color32::WHITE), egui::StrokeKind::Outside);
                }
                if b.width() > 34.0 {
                    p.with_clip_rect(b.shrink(2.0)).text(
                        b.left_center() + egui::vec2(5.0, 0.0),
                        Align2::LEFT_CENTER,
                        item.cue.kind().label(),
                        FontId::proportional(11.0),
                        Color32::BLACK,
                    );
                }

                let resp = ui
                    .interact(b, ui.id().with(("tl-item", id)), Sense::click_and_drag())
                    .on_hover_text(format!("{} @ {}", item.cue.header().name, item.offset.to_clock_string()));
                if resp.clicked() || resp.drag_started() {
                    st.selected = Some(id);
                }
                if resp.drag_started() {
                    st.tl_drag = Some((id, item.offset.as_secs_f32()));
                }
                if resp.dragged() {
                    if let Some((did, raw)) = &mut st.tl_drag {
                        if *did == id {
                            *raw = (*raw + resp.drag_delta().x / zoom).max(0.0);
                            let grid = if st.tl_snap { beat.unwrap_or(0.1) } else { 0.0 };
                            let snapped = if grid > 0.0 { (*raw / grid).round() * grid } else { *raw };
                            item.offset = ShowTime::from_secs_f64(snapped as f64);
                            changed = true;
                        }
                    }
                }
                if resp.drag_stopped() {
                    st.tl_drag = None;
                    drag_ended = true;
                }
            }
            if drag_ended {
                t.items.sort_by_key(|i| i.offset);
            }

            // Playhead.
            if let Some(e) = playhead {
                let x = x_of(e.as_secs_f32());
                p.line_segment([Pos2::new(x, area.top()), Pos2::new(x, area.bottom())], Stroke::new(2.0_f32, theme::stop()));
            }
            // Gutter divider.
            let gx = area.left() + GUTTER - 4.0;
            p.line_segment([Pos2::new(gx, area.top()), Pos2::new(gx, area.bottom())], Stroke::new(1.0_f32, theme::BORDER));
        });
    }
    if playhead.is_some() {
        ui.ctx().request_repaint();
    }
    if changed {
        rt.mark_changed();
    }
}
