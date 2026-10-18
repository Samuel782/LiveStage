//! Distraction-free cue execution table.

use super::widgets::{fmt_dur, menu_heading, menu_item, menu_item_danger, menu_item_enabled, menu_separator, shortcut};
use super::UiState;
use crate::runtime::{CueOp, CueRow, SlideNav};
use crate::view::UiRuntime;
use crate::theme;
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui};
use livestage_core::cue::{Cue, CueKind, FollowMode};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::id::CueId;
use livestage_core::{CueState, ShowTime};

/// What a row's context menu or scrub bar asked for (applied after the loop,
/// when the runtime is no longer borrowed by the row data).
enum RowAction {
    Seek(CueId, ShowTime),
    Op(CueId, CueOp),
    Go(String),
    Standby(String),
    Slides(CueId, SlideNav),
    ToggleArm(CueId),
    Duplicate(CueId),
    Delete(CueId),
    Move(CueId, isize),
}

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
fn list_tabs(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
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

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
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
    let mut actions: Vec<RowAction> = Vec::new();
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
                theme::accent_dim()
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
                p.rect_stroke(body, 3.0, egui::Stroke::new(1.5_f32, theme::accent()), egui::StrokeKind::Inside);
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
                        let (v, c) = match (row.offset, row.timecode) {
                            (Some(o), _) => (format!("@ {}", &o.to_clock_string()[3..]), theme::TEXT_DIM),
                            // Timecode-triggered: the trigger replaces the pre-wait.
                            (None, Some(tc)) => (tc.to_string(), theme::accent()),
                            (None, None) => (secs(row.pre_wait), theme::TEXT_DIM),
                        };
                        cell(&p, rect, *r, v, FontId::monospace(12.0), c, 0.0);
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
                            // The progress bar of a running cue is a scrub bar.
                            if let Some(d) = row.duration.filter(|d| !d.is_zero()) {
                                if let Some(t) = scrub_cell(ui, &p, ar, row, d) {
                                    actions.push(RowAction::Seek(row.id, t));
                                }
                            }
                        }
                        let dur = row.detail.clone().unwrap_or_else(|| fmt_dur(row.duration));
                        let (txt, c) = if running && row.detail.is_some() {
                            (dur, theme::TEXT)
                        } else if running {
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

            if resp.clicked() || resp.secondary_clicked() {
                st.selected = Some(row.id);
            }
            if resp.double_clicked() && row.depth == 0 {
                actions.push(RowAction::Standby(row.number.clone()));
            }
            let resp = resp.on_hover_text("Click: select · Double-click: set as next · Right-click: actions");
            resp.context_menu(|ui| row_menu(ui, row, &mut actions));
        }
        if rows.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No cues yet. Use \"+ Add cue\" or drop media files onto the window.").color(theme::TEXT_DIM));
            });
        }
    });
    for a in actions {
        apply(rt, st, a);
    }
}

/// Click / drag on a running cue's progress bar to seek. Throttled like the
/// inspector scrubber; shows a playhead hint on hover.
fn scrub_cell(ui: &mut Ui, p: &egui::Painter, bar: Rect, row: &CueRow, duration: ShowTime) -> Option<ShowTime> {
    let id = egui::Id::new(("grid-scrub", row.id));
    let resp = ui.interact(bar, id, Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    let to_time = |x: f32| ShowTime::from_secs_f64(((x - bar.left()) / bar.width()).clamp(0.0, 1.0) as f64 * duration.as_secs_f64());
    if let Some(h) = resp.hover_pos() {
        p.line_segment([egui::pos2(h.x, bar.top()), egui::pos2(h.x, bar.bottom())], egui::Stroke::new(1.5_f32, theme::TEXT));
        let t = to_time(h.x);
        p.text(egui::pos2(h.x + 4.0, bar.top() - 1.0), Align2::LEFT_BOTTOM, &t.to_clock_string()[3..], FontId::monospace(11.0), theme::TEXT);
    }
    let pos = resp.interact_pointer_pos().map(|q| to_time(q.x))?;
    let now = ui.input(|i| i.time);
    let last: f64 = ui.data(|d| d.get_temp(id)).unwrap_or(f64::NEG_INFINITY);
    if resp.drag_stopped() || resp.clicked() || now - last >= 0.04 {
        ui.data_mut(|d| d.insert_temp(id, now));
        return Some(pos);
    }
    None
}

/// Right-click menu of a cue row.
fn row_menu(ui: &mut Ui, row: &CueRow, actions: &mut Vec<RowAction>) {
    ui.set_min_width(230.0);
    menu_heading(ui, &format!("Cue {} · {}", row.number, row.kind));
    let top = row.depth == 0;
    let running = row.state.is_active();
    if top && menu_item(ui, "GO this cue now", None).clicked() {
        actions.push(RowAction::Go(row.number.clone()));
    }
    if top && menu_item(ui, "Set as next (standby)", Some("double-click")).clicked() {
        actions.push(RowAction::Standby(row.number.clone()));
    }
    if running {
        menu_separator(ui);
        if row.state == CueState::Paused {
            if menu_item(ui, "Resume", None).clicked() {
                actions.push(RowAction::Op(row.id, CueOp::Resume));
            }
        } else if menu_item(ui, "Pause", None).clicked() {
            actions.push(RowAction::Op(row.id, CueOp::Pause));
        }
        if menu_item(ui, "Restart from top", None).clicked() {
            actions.push(RowAction::Seek(row.id, ShowTime::ZERO));
        }
        if let Some(d) = row.duration.filter(|d| d.as_secs_f64() > 20.0) {
            if menu_item(ui, "Skip to last 10 s", None).clicked() {
                actions.push(RowAction::Seek(row.id, d - ShowTime::from_secs(10)));
            }
        }
        if row.kind == CueKind::Slides.label() {
            if menu_item(ui, "Next slide", Some("→")).clicked() {
                actions.push(RowAction::Slides(row.id, SlideNav::Next));
            }
            if menu_item(ui, "Previous slide", Some("←")).clicked() {
                actions.push(RowAction::Slides(row.id, SlideNav::Previous));
            }
        }
        if menu_item(ui, "Stop (with fade)", None).clicked() {
            actions.push(RowAction::Op(row.id, CueOp::Stop));
        }
    }
    menu_separator(ui);
    if menu_item(ui, if row.armed { "Disarm (skip on GO)" } else { "Arm" }, None).clicked() {
        actions.push(RowAction::ToggleArm(row.id));
    }
    if menu_item_enabled(ui, top, "Duplicate", Some(&shortcut("Cmd+D"))).clicked() {
        actions.push(RowAction::Duplicate(row.id));
    }
    if menu_item_enabled(ui, top, "Move up", None).clicked() {
        actions.push(RowAction::Move(row.id, -1));
    }
    if menu_item_enabled(ui, top, "Move down", None).clicked() {
        actions.push(RowAction::Move(row.id, 1));
    }
    if menu_item(ui, "Copy cue number", None).clicked() {
        ui.ctx().copy_text(row.number.clone());
    }
    menu_separator(ui);
    if top && menu_item_danger(ui, "Delete cue", Some(&shortcut("Cmd+Delete"))).clicked() {
        actions.push(RowAction::Delete(row.id));
    }
}

fn apply(rt: &mut UiRuntime, st: &mut UiState, a: RowAction) {
    let ui_cmd = |rt: &UiRuntime, c: ShowCommand| rt.command(ControlSource::Ui, c);
    match a {
        RowAction::Seek(id, t) => rt.seek_cue(id, t),
        RowAction::Op(id, op) => rt.cue_op(id, op),
        RowAction::Go(n) => ui_cmd(rt, ShowCommand::GoCue { number: n }),
        RowAction::Standby(n) => ui_cmd(rt, ShowCommand::SetStandby { number: n }),
        RowAction::Slides(id, nav) => {
            rt.slide_nav(Some(id), nav);
        }
        RowAction::ToggleArm(id) => {
            if let Some(c) = rt.find_cue_mut(id) {
                let h = c.header_mut();
                h.armed = !h.armed;
                rt.mark_changed();
            }
        }
        RowAction::Duplicate(id) => st.selected = rt.duplicate_cue(id).or(st.selected),
        RowAction::Delete(id) => {
            if let Some(l) = rt.bundle.project.active_list_mut() {
                l.cues.retain(|c| c.header().id != id);
            }
            if st.selected == Some(id) {
                st.selected = None;
            }
            rt.after_structure_edit();
        }
        RowAction::Move(id, dir) => {
            if let Some(l) = rt.bundle.project.active_list_mut() {
                if let Some(i) = l.index_of(id) {
                    let j = i as isize + dir;
                    if j >= 0 && (j as usize) < l.cues.len() {
                        l.cues.swap(i, j as usize);
                    }
                }
            }
            rt.after_structure_edit();
        }
    }
}

fn toolbar(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    let sel_index = st.selected.and_then(|id| rt.bundle.project.active_list().and_then(|l| l.index_of(id)));
    ui.horizontal(|ui| {
        ui.menu_button(RichText::new("+ Add cue").strong(), |ui| {
            ui.set_min_width(200.0);
            menu_heading(ui, "New cue after the selection");
            for kind in [CueKind::Audio, CueKind::Video, CueKind::Timer, CueKind::Slides, CueKind::Dmx, CueKind::Control, CueKind::Timeline] {
                if menu_item(ui, kind.label(), None).clicked() {
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
