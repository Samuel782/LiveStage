//! Distraction-free cue execution table.

use super::widgets::fmt_dur;
use super::UiState;
use crate::runtime::ShowRuntime;
use crate::theme;
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui};
use livestage_core::cue::{Cue, CueKind, FollowMode};
use livestage_core::{CueState, ShowTime};

const ROW_H: f32 = 36.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Number,
    Kind,
    Name,
    Pre,
    Action,
    Post,
    Follow,
    State,
}

impl Col {
    fn title(self) -> &'static str {
        match self {
            Col::Number => "#",
            Col::Kind => "TYPE",
            Col::Name => "NAME",
            Col::Pre => "PRE-WAIT",
            Col::Action => "ACTION",
            Col::Post => "POST-WAIT",
            Col::Follow => "FOLLOW",
            Col::State => "STATE",
        }
    }
    /// Fixed width; `None` = takes the remaining space.
    fn width(self) -> Option<f32> {
        match self {
            Col::Number => Some(78.0),
            Col::Kind => Some(72.0),
            Col::Name => None,
            Col::Pre => Some(88.0),
            Col::Action => Some(196.0),
            Col::Post => Some(88.0),
            Col::Follow => Some(74.0),
            Col::State => Some(82.0),
        }
    }
}

/// Columns that fit `width`; secondary columns drop out on narrow panels.
fn columns(width: f32) -> Vec<(Col, Rect)> {
    let mut cols = vec![Col::Number, Col::Kind, Col::Name, Col::Pre, Col::Action, Col::Post, Col::Follow, Col::State];
    let min_name = 160.0;
    for drop in [Col::Follow, Col::Post, Col::Kind, Col::Pre] {
        let fixed: f32 = cols.iter().filter_map(|c| c.width()).sum();
        if fixed + min_name + 12.0 <= width {
            break;
        }
        cols.retain(|c| *c != drop);
    }
    let fixed: f32 = cols.iter().filter_map(|c| c.width()).sum();
    let flex = (width - fixed - 12.0).max(min_name);
    let mut x = 12.0;
    cols.into_iter()
        .map(|c| {
            let w = c.width().unwrap_or(flex);
            let r = Rect::from_min_size(egui::pos2(x, 0.0), egui::vec2(w, 1.0));
            x += w;
            (c, r)
        })
        .collect()
}

/// Draws `text` clipped to the cell (never bleeds into the next column).
fn cell(p: &egui::Painter, row: Rect, col: Rect, text: impl ToString, font: FontId, color: Color32, indent: f32) {
    let r = Rect::from_min_max(egui::pos2(row.left() + col.left(), row.top()), egui::pos2(row.left() + col.right() - 6.0, row.bottom()));
    p.with_clip_rect(r).text(egui::pos2(r.left() + indent, r.center().y), Align2::LEFT_CENTER, text.to_string(), font, color);
}

fn secs(t: ShowTime) -> String {
    if t.is_zero() {
        String::new()
    } else {
        format!("{:.2} s", t.as_secs_f64())
    }
}

/// Cue list selector: GO, standby and the grid follow the active list;
/// panic and stop-all always reach every list.
fn list_tabs(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState) {
    let lists: Vec<(livestage_core::CueListId, String, bool)> = rt
        .bundle
        .project
        .cue_lists
        .iter()
        .map(|l| (l.id, l.name.clone(), l.is_running()))
        .collect();
    let active = rt.bundle.project.active_cue_list;
    ui.horizontal_wrapped(|ui| {
        for (id, name, running) in &lists {
            if let Some((rid, text)) = &mut st.renaming_list {
                if rid == id {
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(140.0));
                    r.request_focus();
                    if r.lost_focus() {
                        let new_name = text.trim().to_string();
                        if let Some(l) = rt.bundle.project.cue_lists.iter_mut().find(|l| l.id == *id) {
                            if !new_name.is_empty() {
                                l.name = new_name;
                            }
                        }
                        st.renaming_list = None;
                        rt.mark_changed();
                    }
                    continue;
                }
            }
            let label = if *running { RichText::new(format!("{name}  (running)")).color(theme::active()) } else { RichText::new(name) };
            let r = ui.selectable_label(active == Some(*id), label).on_hover_text("Double-click to rename");
            if r.clicked() && active != Some(*id) {
                rt.bundle.project.active_cue_list = Some(*id);
                st.selected = None;
            }
            if r.double_clicked() {
                st.renaming_list = Some((*id, name.clone()));
            }
            r.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    st.renaming_list = Some((*id, name.clone()));
                    ui.close_menu();
                }
                let can_delete = lists.len() > 1 && !running;
                if ui.add_enabled(can_delete, egui::Button::new("Delete list")).clicked() {
                    rt.bundle.project.cue_lists.retain(|l| l.id != *id);
                    if rt.bundle.project.active_cue_list == Some(*id) {
                        rt.bundle.project.active_cue_list = rt.bundle.project.cue_lists.first().map(|l| l.id);
                    }
                    st.selected = None;
                    rt.mark_changed();
                    ui.close_menu();
                }
            });
        }
        if ui.small_button("+ List").clicked() {
            let n = lists.len() + 1;
            rt.add_cue_list(&format!("Cue List {n}"));
            st.selected = None;
        }
    });
}

pub fn show(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState) {
    list_tabs(ui, rt, st);
    toolbar(ui, rt, st);
    ui.add_space(4.0);

    // Header.
    let (hdr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
    let cols = columns(hdr.width());
    let p = ui.painter();
    p.rect_filled(hdr, 3.0, theme::PANEL_RECESSED);
    for (c, r) in &cols {
        cell(p, hdr, *r, c.title(), FontId::proportional(11.0), theme::TEXT_DIM, 0.0);
    }

    let rows = rt.cue_rows();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for row in &rows {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
            let cols = columns(rect.width());
            let selected = st.selected == Some(row.id);
            let running = row.state.is_active();
            let bg = if row.is_standby {
                Color32::from_rgb(0x3A, 0x2F, 0x0C)
            } else if running {
                Color32::from_rgb(0x14, 0x2E, 0x19)
            } else if selected {
                theme::ACCENT_DIM
            } else if row.depth > 0 {
                theme::PANEL_RECESSED
            } else {
                theme::PANEL_RAISED
            };
            let p = ui.painter_at(rect);
            let body = rect.shrink2(egui::vec2(0.0, 1.0));
            p.rect_filled(body, 3.0, bg);
            let stripe = if row.is_standby { Some(theme::standby()) } else { theme::state_color(row.state) };
            if let Some(c) = stripe {
                p.rect_filled(Rect::from_min_size(body.min, egui::vec2(5.0, body.height())), 0.0, c);
            }
            if selected {
                p.rect_stroke(body, 3.0, egui::Stroke::new(1.5_f32, theme::go()), egui::StrokeKind::Inside);
            }

            let text = if row.armed { theme::TEXT } else { theme::TEXT_DIM };
            let indent = row.depth as f32 * 14.0;
            for (c, r) in &cols {
                match c {
                    Col::Number => {
                        let n = if row.depth > 0 { format!("↳ {}", row.number) } else { row.number.clone() };
                        cell(&p, rect, *r, n, FontId::monospace(if row.depth > 0 { 13.0 } else { 17.0 }), text, indent);
                    }
                    Col::Kind => cell(&p, rect, *r, row.kind, FontId::proportional(12.0), theme::TEXT_DIM, 0.0),
                    Col::Name => cell(&p, rect, *r, &row.name, FontId::proportional(16.0), text, indent),
                    Col::Pre => {
                        let v = match row.offset {
                            Some(o) => format!("@ {}", &o.to_clock_string()[3..]),
                            None => secs(row.pre_wait),
                        };
                        cell(&p, rect, *r, v, FontId::monospace(13.0), theme::TEXT_DIM, 0.0);
                    }
                    Col::Action => {
                        let ar = Rect::from_min_max(
                            egui::pos2(rect.left() + r.left(), rect.top() + 6.0),
                            egui::pos2(rect.left() + r.right() - 8.0, rect.bottom() - 6.0),
                        );
                        if let Some(pr) = row.progress.filter(|_| running) {
                            p.rect_filled(ar, 3.0, theme::PANEL_RECESSED);
                            let mut fill = ar;
                            fill.set_width(ar.width() * pr);
                            p.rect_filled(fill, 3.0, theme::active().gamma_multiply(0.45));
                        }
                        let dur = fmt_dur(row.duration);
                        let (txt, c) = if running {
                            (format!("{} / {dur}", &row.elapsed.to_clock_string()[3..]), theme::TEXT)
                        } else if matches!(row.state, CueState::Completed | CueState::Stopped) {
                            (format!("{} / {dur}", &row.elapsed.to_clock_string()[3..]), theme::TEXT_DIM)
                        } else {
                            (dur, theme::TEXT_DIM)
                        };
                        cell(&p, rect, *r, txt, FontId::monospace(14.0), c, 6.0);
                    }
                    Col::Post => cell(&p, rect, *r, secs(row.post_wait), FontId::monospace(13.0), theme::TEXT_DIM, 0.0),
                    Col::Follow => {
                        let f = match row.follow {
                            FollowMode::None => "",
                            FollowMode::AutoContinue => "Continue",
                            FollowMode::AutoFollow => "Follow",
                        };
                        cell(&p, rect, *r, f, FontId::proportional(12.0), theme::TEXT_DIM, 0.0);
                    }
                    Col::State => {
                        let (label, c) = if row.is_standby && !running {
                            ("STANDBY", theme::standby())
                        } else {
                            (row.state.label(), theme::state_color(row.state).unwrap_or(theme::TEXT_DIM))
                        };
                        cell(&p, rect, *r, label, FontId::proportional(12.0), c, 0.0);
                    }
                }
            }

            if resp.clicked() {
                st.selected = Some(row.id);
            }
            if resp.double_clicked() && row.depth == 0 {
                rt.command(livestage_control::ControlSource::Ui, livestage_control::ShowCommand::SetStandby { number: row.number.clone() });
            }
            resp.on_hover_text("Click: select · Double-click: set as next (standby)");
        }
        if rows.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No cues yet. Use \"+ Add cue\" or drop media files onto the window.").color(theme::TEXT_DIM));
            });
        }
    });
}

fn toolbar(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState) {
    let sel_index = st.selected.and_then(|id| rt.bundle.project.active_list().and_then(|l| l.index_of(id)));
    ui.horizontal(|ui| {
        ui.menu_button(RichText::new("+ Add cue").strong(), |ui| {
            for kind in [CueKind::Audio, CueKind::Video, CueKind::Timer, CueKind::Dmx, CueKind::Control, CueKind::Timeline] {
                if ui.button(kind.label()).clicked() {
                    let number = rt.next_cue_number();
                    let cue = crate::demo::new_cue(&rt.bundle.project, kind, &number);
                    st.selected = Some(cue.header().id);
                    if let Some(l) = rt.bundle.project.active_list_mut() {
                        // Insert after the selected cue, else append.
                        let at = sel_index.map_or(l.cues.len(), |i| (i + 1).min(l.cues.len()));
                        l.cues.insert(at, cue);
                    }
                    rt.after_structure_edit();
                    ui.close_menu();
                }
            }
        });
        ui.add_enabled_ui(sel_index.is_some(), |ui| {
            if ui.button("Move up").clicked() {
                if let (Some(i), Some(l)) = (sel_index, rt.bundle.project.active_list_mut()) {
                    if i > 0 {
                        l.cues.swap(i, i - 1);
                    }
                }
                rt.after_structure_edit();
            }
            if ui.button("Move down").clicked() {
                if let (Some(i), Some(l)) = (sel_index, rt.bundle.project.active_list_mut()) {
                    if i + 1 < l.cues.len() {
                        l.cues.swap(i, i + 1);
                    }
                }
                rt.after_structure_edit();
            }
            if ui.button(RichText::new("Delete").color(theme::stop())).on_hover_text("Delete the selected cue (Undo restores it)").clicked() {
                if let (Some(i), Some(l)) = (sel_index, rt.bundle.project.active_list_mut()) {
                    l.cues.remove(i);
                    st.selected = None;
                }
                rt.after_structure_edit();
            }
        });
    });
}
