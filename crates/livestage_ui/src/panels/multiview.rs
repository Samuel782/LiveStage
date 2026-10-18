//! Multiview / control booth: a configurable monitor wall. Any cell can
//! show a live output canvas, the program cue (clocks + scrub bar), the next
//! cue with a preview of its content, the running presentation's current
//! and upcoming slide (with text and speaker notes), or a clock.

use super::widgets::{menu_heading, menu_item, menu_separator, scrub_bar};
use super::{NdiStatus, Previews, UiState};
use crate::runtime::ShowRuntime;
use crate::theme;
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui};
use livestage_core::action::VisualContent;
use livestage_core::cue::{AnyCue, Cue, CueExt, CueKind};
use livestage_core::id::{CueId, OutputId};
use livestage_core::multiview::{MultiviewLayout, MultiviewSource};
use livestage_core::project::AssetRef;
use livestage_core::ShowTime;

const CAPTION: f32 = 24.0;
/// Below this picture height cells switch to a side-by-side compact layout.
const COMPACT: f32 = 170.0;
const GAP: f32 = 8.0;

/// Layout presets offered in the toolbar: (label, rows, cols, featured).
const PRESETS: [(&str, u8, u8, bool); 8] = [
    ("1 × 1", 1, 1, false),
    ("1 × 2", 1, 2, false),
    ("2 × 2", 2, 2, false),
    ("2 × 3", 2, 3, false),
    ("3 × 3", 3, 3, false),
    ("4 × 4", 4, 4, false),
    ("Featured + 5", 3, 3, true),
    ("Featured + 7", 3, 4, true),
];

enum Action {
    Seek(CueId, ShowTime),
    Assign(usize, MultiviewSource),
    Grid(u8, u8, bool),
    Auto,
}

pub fn show(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState, previews: &Previews, ndi: &NdiStatus) {
    let layout = rt.bundle.project.multiview.clone().unwrap_or_else(|| MultiviewLayout::auto(&rt.bundle.project.displays));
    let is_auto = rt.bundle.project.multiview.is_none();
    let mut actions = Vec::new();
    toolbar(ui, &layout, is_auto, &mut actions);
    ui.add_space(4.0);

    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let (cols, rows) = (layout.cols.max(1) as f32, layout.rows.max(1) as f32);
    let cw = (area.width() - GAP * (cols - 1.0)) / cols;
    let ch = (area.height() - GAP * (rows - 1.0)) / rows;
    for (i, (c, r, sc, sr)) in layout.placements().into_iter().enumerate() {
        let min = area.min + egui::vec2(c as f32 * (cw + GAP), r as f32 * (ch + GAP));
        let size = egui::vec2(cw * sc as f32 + GAP * (sc as f32 - 1.0), ch * sr as f32 + GAP * (sr as f32 - 1.0));
        let rect = Rect::from_min_size(min, size);
        let source = layout.cells.get(i).copied().unwrap_or_default();
        // Right-click anywhere on the cell to choose its feed. Registered
        // first so widgets inside the cell (the scrub bar) sit on top of it.
        let resp = ui.interact(rect, egui::Id::new(("mv-menu", i)), Sense::click());
        let mut cell_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("mv-cell", i)));
        cell(&mut cell_ui, i, rect, source, rt, st, previews, ndi, &mut actions);
        resp.context_menu(|ui| source_menu(ui, i, source, rt, &mut actions));
    }

    for a in actions {
        match a {
            Action::Seek(id, t) => rt.seek_cue(id, t),
            Action::Assign(i, s) => {
                let mut l = layout.clone();
                if i < l.cells.len() {
                    l.cells[i] = s;
                }
                rt.bundle.project.multiview = Some(l);
                rt.mark_changed();
            }
            Action::Grid(r, c, f) => {
                let mut l = layout.clone();
                l.set_grid(r, c, f);
                rt.bundle.project.multiview = Some(l);
                rt.mark_changed();
            }
            Action::Auto => {
                rt.bundle.project.multiview = None;
                rt.mark_changed();
            }
        }
    }
}

fn toolbar(ui: &mut Ui, layout: &MultiviewLayout, is_auto: bool, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Layout").color(theme::TEXT_DIM));
        for (label, r, c, f) in PRESETS {
            let active = layout.rows == r && layout.cols == c && layout.has_feature() == f;
            if ui.selectable_label(active, label).clicked() && !active {
                actions.push(Action::Grid(r, c, f));
            }
        }
        ui.separator();
        let (mut r, mut c, mut f) = (layout.rows, layout.cols, layout.feature_first);
        ui.label(RichText::new("Custom").color(theme::TEXT_DIM));
        let mut changed = ui.add(egui::DragValue::new(&mut r).range(1..=MultiviewLayout::MAX).suffix(" rows")).changed();
        changed |= ui.add(egui::DragValue::new(&mut c).range(1..=MultiviewLayout::MAX).suffix(" cols")).changed();
        changed |= ui.checkbox(&mut f, "featured").on_hover_text("First cell spans 2 × 2").changed();
        if changed {
            actions.push(Action::Grid(r, c, f));
        }
        ui.separator();
        if ui.add_enabled(!is_auto, egui::Button::new("Auto")).on_hover_text("Every output plus Program / Next / Slides / Clock").clicked() {
            actions.push(Action::Auto);
        }
        ui.label(RichText::new("Right-click a cell to choose its feed").size(12.0).color(theme::TEXT_DIM));
    });
}

fn source_label(s: MultiviewSource, rt: &ShowRuntime) -> String {
    match s {
        MultiviewSource::Empty => "Empty".into(),
        MultiviewSource::Output { id } => rt.bundle.project.display(id).map_or("Output (removed)".into(), |d| d.name.clone()),
        MultiviewSource::Program => "Program (running cue)".into(),
        MultiviewSource::Next => "Next cue (standby)".into(),
        MultiviewSource::NextSlide => "Presentation (current / next slide)".into(),
        MultiviewSource::Clock => "Clock".into(),
    }
}

fn source_menu(ui: &mut Ui, i: usize, current: MultiviewSource, rt: &ShowRuntime, actions: &mut Vec<Action>) {
    ui.set_min_width(250.0);
    menu_heading(ui, &format!("Cell {}", i + 1));
    let mut item = |ui: &mut Ui, s: MultiviewSource, label: String| {
        let mark = if s == current { "✔" } else { "" };
        if menu_item(ui, &label, Some(mark)).clicked() {
            actions.push(Action::Assign(i, s));
        }
    };
    menu_heading(ui, "Outputs");
    for d in &rt.bundle.project.displays {
        item(ui, MultiviewSource::Output { id: d.id }, d.name.clone());
    }
    menu_separator(ui);
    menu_heading(ui, "Booth feeds");
    for s in [MultiviewSource::Program, MultiviewSource::Next, MultiviewSource::NextSlide, MultiviewSource::Clock] {
        item(ui, s, source_label(s, rt));
    }
    menu_separator(ui);
    item(ui, MultiviewSource::Empty, "Empty".into());
}

#[allow(clippy::too_many_arguments)]
fn cell(
    ui: &mut Ui,
    i: usize,
    rect: Rect,
    source: MultiviewSource,
    rt: &ShowRuntime,
    st: &mut UiState,
    previews: &Previews,
    ndi: &NdiStatus,
    actions: &mut Vec<Action>,
) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 5.0, Color32::BLACK);
    let picture = Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.bottom() - CAPTION));
    let (title, accent, badges): (String, Color32, Vec<(String, Color32)>) = match source {
        MultiviewSource::Empty => {
            p.text(picture.center(), Align2::CENTER_CENTER, "Right-click to assign", FontId::proportional(13.0), theme::TEXT_DIM);
            (format!("{}", i + 1), theme::BORDER, vec![])
        }
        MultiviewSource::Output { id } => output_cell(&p, picture, id, rt, previews, ndi),
        MultiviewSource::Program => program_cell(ui, picture, rt, st, previews, actions),
        MultiviewSource::Next => next_cell(&p, picture, rt, st, previews),
        MultiviewSource::NextSlide => slides_cell(&p, picture, rt, st),
        MultiviewSource::Clock => {
            let now = chrono::Local::now();
            let size = (picture.height() * 0.32).min(picture.width() * 0.16);
            p.text(picture.center() - egui::vec2(0.0, size * 0.15), Align2::CENTER_CENTER, now.format("%H:%M:%S").to_string(), theme::clock_font(size), theme::TEXT);
            p.text(
                picture.center() + egui::vec2(0.0, size * 0.62),
                Align2::CENTER_CENTER,
                now.format("%A %d %B").to_string(),
                FontId::proportional((size * 0.22).max(11.0)),
                theme::TEXT_DIM,
            );
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            ("CLOCK".into(), theme::BORDER, vec![])
        }
    };
    p.rect_stroke(rect, 5.0, Stroke::new(if accent == theme::BORDER { 1.0_f32 } else { 2.0 }, accent), egui::StrokeKind::Inside);
    // Caption strip (tally-style colour on the left edge).
    let cap = Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - CAPTION), rect.max);
    p.rect_filled(cap, egui::CornerRadius { nw: 0, ne: 0, sw: 5, se: 5 }, theme::PANEL_RECESSED);
    p.rect_filled(Rect::from_min_size(cap.min, egui::vec2(4.0, cap.height())), 0.0, accent);
    let cp = ui.painter_at(cap.shrink2(egui::vec2(10.0, 0.0)));
    cp.text(cap.left_center() + egui::vec2(10.0, 0.0), Align2::LEFT_CENTER, &title, FontId::proportional(13.0), theme::TEXT);
    let mut x = cap.right() - 10.0;
    for (text, color) in badges.into_iter().rev() {
        let r = cp.text(egui::pos2(x, cap.center().y), Align2::RIGHT_CENTER, text, FontId::proportional(11.5), color);
        x = r.left() - 10.0;
    }
}

/// Paints a texture contain-fitted into `area`; returns the image rect.
fn fit_image(p: &egui::Painter, area: Rect, tex: egui::TextureId, size: egui::Vec2, tint: Color32) -> Rect {
    let scale = (area.width() / size.x.max(1.0)).min(area.height() / size.y.max(1.0));
    let r = Rect::from_center_size(area.center(), size * scale);
    p.image(tex, r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
    r
}

fn output_cell(p: &egui::Painter, picture: Rect, id: OutputId, rt: &ShowRuntime, previews: &Previews, ndi: &NdiStatus) -> (String, Color32, Vec<(String, Color32)>) {
    let Some(d) = rt.bundle.project.display(id) else {
        p.text(picture.center(), Align2::CENTER_CENTER, "Output removed", FontId::proportional(13.0), theme::TEXT_DIM);
        return ("—".into(), theme::BORDER, vec![]);
    };
    match previews.get(&id) {
        Some((tex, (w, h))) => {
            fit_image(p, picture.shrink(2.0), *tex, egui::vec2(*w as f32, *h as f32), Color32::WHITE);
        }
        None => {
            p.text(picture.center(), Align2::CENTER_CENTER, "Output disabled", FontId::proportional(13.0), theme::TEXT_DIM);
        }
    }
    let mut badges = Vec::new();
    let live = rt.outputs_live && d.enabled;
    if live {
        badges.push(("LIVE".to_string(), theme::stop()));
    }
    if let Some(n) = ndi.get(&id) {
        badges.push((format!("NDI {n}"), if *n > 0 { theme::go() } else { theme::TEXT_DIM }));
    }
    if let Some([w, h]) = d.canvas_size {
        badges.push((format!("{w}×{h}"), theme::TEXT_DIM));
    }
    (d.name.clone(), if live { theme::stop() } else { theme::BORDER }, badges)
}

/// The output a visual cue plays on.
fn cue_output(c: &AnyCue) -> Option<OutputId> {
    match c {
        AnyCue::Video(v) => Some(v.output),
        AnyCue::Timer(t) => Some(t.output),
        AnyCue::Slides(s) => Some(s.output),
        _ => None,
    }
}

fn program_cell(
    ui: &mut Ui,
    picture: Rect,
    rt: &ShowRuntime,
    st: &mut UiState,
    previews: &Previews,
    actions: &mut Vec<Action>,
) -> (String, Color32, Vec<(String, Color32)>) {
    let p = ui.painter_at(picture);
    let Some((cue, elapsed)) = rt.program_cue() else {
        p.text(picture.center(), Align2::CENTER_CENTER, "Nothing running", FontId::proportional(16.0), theme::TEXT_DIM);
        return ("PROGRAM".into(), theme::BORDER, vec![]);
    };
    let h = cue.header();
    let compact = picture.height() < COMPACT;
    let bar_h = if cue.duration().is_some() { 30.0 } else { 0.0 };
    // Picture: the output the cue plays on. Compact cells put it on the left.
    let pic = if compact {
        let w = ((picture.height() - bar_h - 8.0) * 16.0 / 9.0).min(picture.width() * 0.4);
        Rect::from_min_size(picture.min + egui::vec2(6.0, 6.0), egui::vec2(w, picture.height() - bar_h - 12.0))
    } else {
        Rect::from_min_max(picture.min + egui::vec2(6.0, 6.0), egui::pos2(picture.right() - 6.0, picture.top() + picture.height() * 0.55))
    };
    let mut has_picture = false;
    if let Some((tex, (w, hgt))) = cue_output(cue).and_then(|o| previews.get(&o)) {
        fit_image(&p, pic, *tex, egui::vec2(*w as f32, *hgt as f32), Color32::WHITE);
        has_picture = true;
    } else if let AnyCue::Audio(a) = cue {
        let tracks: Vec<&str> = a.tracks.iter().map(|t| t.asset.file_name()).collect();
        let galley = p.layout(format!("♪  {}", tracks.join(" · ")), FontId::proportional(13.0), theme::TEXT_DIM, pic.width() - 8.0);
        p.galley(pic.center() - galley.size() / 2.0, galley, theme::TEXT_DIM);
        has_picture = compact;
    }
    // Text column: beside the picture (compact) or below it.
    let (text_left, text_top) = if compact {
        (if has_picture { pic.right() + 12.0 } else { picture.left() + 12.0 }, picture.top() + 8.0)
    } else {
        (picture.left() + 12.0, if has_picture { pic.bottom() + 6.0 } else { picture.top() + picture.height() * 0.2 })
    };
    let name_size = if compact { 14.0 } else { (picture.height() * 0.075).clamp(13.0, 26.0) };
    p.text(egui::pos2(text_left, text_top), Align2::LEFT_TOP, format!("{}  {}", h.number, h.name), FontId::proportional(name_size), theme::TEXT);
    let clock = if compact {
        ((picture.height() - bar_h) * 0.32).clamp(14.0, 34.0)
    } else {
        (picture.height() * 0.14).clamp(16.0, 56.0)
    };
    let y = text_top + name_size + 6.0;
    p.text(egui::pos2(text_left, y), Align2::LEFT_TOP, elapsed.to_clock_string(), theme::clock_font(clock), theme::active());
    if let Some(d) = cue.duration() {
        let (rx, ry) = if compact { (picture.right() - 12.0, y + clock + 4.0) } else { (picture.right() - 12.0, y) };
        p.text(
            egui::pos2(rx, ry),
            if compact { Align2::RIGHT_BOTTOM } else { Align2::RIGHT_TOP },
            format!("-{}", (d - elapsed).to_clock_string()),
            theme::clock_font(if compact { clock * 0.7 } else { clock }),
            if d - elapsed < ShowTime::from_secs(10) { theme::stop() } else { theme::TEXT },
        );
        // Scrub bar along the bottom of the cell.
        let bar = Rect::from_min_max(egui::pos2(picture.left() + 12.0, picture.bottom() - 30.0), egui::pos2(picture.right() - 12.0, picture.bottom() - 8.0));
        let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar).id_salt(("mv-scrub", h.id)));
        if let Some(t) = scrub_bar(&mut bar_ui, ("mv-scrub", h.id), elapsed, d, 20.0, theme::active()) {
            actions.push(Action::Seek(h.id, t));
        }
    }
    let _ = st;
    let state = cue.state();
    let badges = vec![(cue.kind().label().to_uppercase(), theme::TEXT_DIM), (state.label().to_string(), theme::state_color(state).unwrap_or(theme::TEXT_DIM))];
    ("PROGRAM".into(), theme::active(), badges)
}

/// Bundle path of the picture that best previews a cue, and the media time to grab.
fn preview_asset(c: &AnyCue) -> Option<(AssetRef, ShowTime)> {
    match c {
        AnyCue::Video(v) => match &v.content {
            // A little way in: many clips start on black.
            VisualContent::Video { asset, .. } => Some((asset.clone(), ShowTime::from_millis(1_000))),
            VisualContent::Image { asset } => Some((asset.clone(), ShowTime::ZERO)),
            _ => None,
        },
        AnyCue::Slides(s) => s.deck.slides.get(s.start_slide).map(|sl| (sl.image.clone(), ShowTime::ZERO)),
        AnyCue::Timeline(t) => t.items.iter().find_map(|i| preview_asset(&i.cue)),
        _ => None,
    }
}

/// Draws the thumbnail of `asset` (or a waiting / failed note) contain-fitted into `area`.
fn thumb(p: &egui::Painter, area: Rect, rt: &ShowRuntime, st: &mut UiState, asset: &AssetRef, at: ShowTime) -> bool {
    let Ok(path) = rt.bundle.resolve(asset) else { return false };
    match st.thumbs.get(&path, at) {
        Some(tex) => {
            fit_image(p, area, tex.id(), tex.size_vec2(), Color32::WHITE);
            true
        }
        None => {
            let msg = if st.thumbs.failed(&path, at) { "No preview" } else { "Loading preview…" };
            p.text(area.center(), Align2::CENTER_CENTER, msg, FontId::proportional(12.0), theme::TEXT_DIM);
            false
        }
    }
}

/// One-line description of a cue's content for previews without pictures.
fn describe(c: &AnyCue) -> String {
    match c {
        AnyCue::Audio(a) => {
            let names: Vec<&str> = a.tracks.iter().map(|t| t.asset.file_name()).collect();
            let click = a.click.as_ref().map_or(String::new(), |k| format!(" + click {} bpm", k.bpm));
            format!("♪ {}{click}", if names.is_empty() { "click only".to_string() } else { names.join(" · ") })
        }
        AnyCue::Video(v) => match &v.content {
            VisualContent::Text(t) => t.text.clone(),
            VisualContent::Ndi { source_name } => format!("NDI: {source_name}"),
            VisualContent::Color { .. } => "Colour fill".into(),
            VisualContent::TestPattern => "Test pattern".into(),
            _ => String::new(),
        },
        AnyCue::Timer(t) => match t.timer.mode {
            livestage_core::action::TimerMode::CountDown { from } => format!("Countdown {}", from.to_timer_string()),
            livestage_core::action::TimerMode::CountUp => "Count-up timer".into(),
            livestage_core::action::TimerMode::TimeOfDay => "Time of day".into(),
        },
        AnyCue::Dmx(d) => format!("Lighting look · {} channels · {:.1} s fade", d.channels.len() + d.attributes.len(), d.fade.as_secs_f64()),
        AnyCue::Control(c) => match &c.action {
            livestage_core::action::ControlAction::SendOsc { host, port, address, .. } => format!("OSC {address} → {host}:{port}"),
            livestage_core::action::ControlAction::SendMidi { bytes, .. } => format!("MIDI {bytes:02X?}"),
            livestage_core::action::ControlAction::Command(cmd) => format!("Command {cmd:?}"),
            livestage_core::action::ControlAction::Marker { text } => format!("Marker: {text}"),
        },
        AnyCue::Timeline(t) => format!("Timeline · {} items", t.items.len()),
        AnyCue::Slides(s) => s.deck.slides.get(s.start_slide).map_or(String::new(), |sl| sl.text.clone()),
    }
}

fn next_cell(p: &egui::Painter, picture: Rect, rt: &ShowRuntime, st: &mut UiState, previews: &Previews) -> (String, Color32, Vec<(String, Color32)>) {
    let Some(cue) = rt.standby_cue() else {
        p.text(picture.center(), Align2::CENTER_CENTER, "End of list", FontId::proportional(16.0), theme::TEXT_DIM);
        return ("NEXT".into(), theme::BORDER, vec![]);
    };
    let h = cue.header();
    let compact = picture.height() < COMPACT;
    let pic = if compact {
        let w = ((picture.height() - 12.0) * 16.0 / 9.0).min(picture.width() * 0.45);
        Rect::from_min_size(picture.min + egui::vec2(6.0, 6.0), egui::vec2(w, picture.height() - 12.0))
    } else {
        Rect::from_min_max(picture.min + egui::vec2(6.0, 6.0), egui::pos2(picture.right() - 6.0, picture.top() + picture.height() * 0.62))
    };
    let shown = match preview_asset(cue) {
        Some((asset, at)) => thumb(p, pic, rt, st, &asset, at),
        None => false,
    };
    let text = describe(cue);
    if !shown && preview_asset(cue).is_none() {
        // Timers / text: show the words as they will read; others a summary.
        let size = if matches!(cue, AnyCue::Timer(_)) { (pic.height() * 0.4).min(64.0) } else { 15.0 };
        let galley = p.layout(text.clone(), FontId::proportional(size), theme::TEXT, pic.width() - 20.0);
        p.galley(pic.center() - galley.size() / 2.0, galley, theme::TEXT);
    }
    let _ = previews;
    let name_size = if compact { 15.0 } else { (picture.height() * 0.07).clamp(13.0, 24.0) };
    let (tx, ty) = if compact { (pic.right() + 12.0, picture.top() + 8.0) } else { (picture.left() + 12.0, pic.bottom() + 6.0) };
    let text_w = picture.right() - tx - 8.0;
    let name = p.layout(format!("{}  {}", h.number, h.name), FontId::proportional(name_size), theme::standby(), text_w);
    let name_h = name.size().y;
    p.galley(egui::pos2(tx, ty), name, theme::standby());
    if shown && !text.is_empty() {
        let first = text.lines().next().unwrap_or_default();
        let g = p.layout(first.to_string(), FontId::proportional(12.5), theme::TEXT_DIM, text_w);
        p.with_clip_rect(picture).galley(egui::pos2(tx, ty + name_h + 6.0), g, theme::TEXT_DIM);
    }
    let mut badges = vec![(cue.kind().label().to_uppercase(), theme::TEXT_DIM)];
    if let Some(d) = cue.duration() {
        badges.push((d.to_clock_string()[3..].to_string(), theme::TEXT_DIM));
    }
    if !h.pre_wait.is_zero() {
        badges.push((format!("pre {:.1}s", h.pre_wait.as_secs_f64()), theme::TEXT_DIM));
    }
    ("NEXT".into(), theme::standby(), badges)
}

fn slides_cell(p: &egui::Painter, picture: Rect, rt: &ShowRuntime, st: &mut UiState) -> (String, Color32, Vec<(String, Color32)>) {
    // The running presentation, else the next one on standby.
    let running = rt.current_slides().and_then(|id| rt.find_cue(id));
    let (cue, is_running) = match running {
        Some(c) => (c, true),
        None => match rt.standby_cue().filter(|c| c.kind() == CueKind::Slides) {
            Some(c) => (c, false),
            None => {
                p.text(picture.center(), Align2::CENTER_CENTER, "No presentation running", FontId::proportional(15.0), theme::TEXT_DIM);
                return ("SLIDES".into(), theme::BORDER, vec![]);
            }
        },
    };
    let AnyCue::Slides(s) = cue else { return ("SLIDES".into(), theme::BORDER, vec![]) };
    let n = s.slide_count();
    let current = if is_running { s.current } else { s.start_slide };
    // Upcoming slide large on the right, current small on the left (presenter view).
    let band = Rect::from_min_max(picture.min + egui::vec2(6.0, 6.0), egui::pos2(picture.right() - 6.0, picture.top() + picture.height() * 0.62));
    let split = band.left() + band.width() * 0.36;
    let left = Rect::from_min_max(band.min, egui::pos2(split - 4.0, band.bottom()));
    let right = Rect::from_min_max(egui::pos2(split + 4.0, band.top()), band.max);
    let label_y = band.bottom() + 3.0;
    if let Some(sl) = s.deck.slides.get(current) {
        thumb(p, left, rt, st, &sl.image, ShowTime::ZERO);
        p.text(egui::pos2(left.left(), label_y), Align2::LEFT_TOP, format!("NOW  {} / {n}", current + 1), FontId::proportional(12.0), theme::active());
    }
    match s.deck.slides.get(current + 1) {
        Some(next) => {
            thumb(p, right, rt, st, &next.image, ShowTime::ZERO);
            p.text(egui::pos2(right.left(), label_y), Align2::LEFT_TOP, format!("NEXT  {} / {n}", current + 2), FontId::proportional(12.0), theme::standby());
        }
        None => {
            p.text(right.center(), Align2::CENTER_CENTER, "End of presentation", FontId::proportional(14.0), theme::TEXT_DIM);
        }
    }
    // Text of the upcoming slide and the current slide's speaker notes.
    let text_rect = Rect::from_min_max(egui::pos2(picture.left() + 10.0, label_y + 20.0), picture.max - egui::vec2(10.0, 4.0));
    let mut lines = String::new();
    if let Some(next) = s.deck.slides.get(current + 1) {
        if !next.text.is_empty() {
            lines.push_str(&next.text.replace('\n', " · "));
        }
    }
    if let Some(cur) = s.deck.slides.get(current).filter(|c| !c.notes.is_empty()) {
        if !lines.is_empty() {
            lines.push_str("\n\n");
        }
        lines.push_str(&format!("Notes: {}", cur.notes));
    }
    if !lines.is_empty() {
        let galley = p.layout(lines, FontId::proportional(12.5), theme::TEXT, text_rect.width());
        p.with_clip_rect(text_rect).galley(text_rect.min, galley, theme::TEXT);
    }
    let badges = vec![(if is_running { "RUNNING".to_string() } else { "STANDBY".into() }, if is_running { theme::active() } else { theme::standby() })];
    (format!("SLIDES · {}", s.header.name), if is_running { theme::active() } else { theme::standby() }, badges)
}
