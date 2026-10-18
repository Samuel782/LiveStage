//! Transport bar (GO / NEXT / STOP / PANIC / countdown / clock / outputs),
//! menu bar and status bar.
//!
//! The transport is laid out in fixed regions computed from the available
//! width. Optional regions are dropped on narrow windows rather than allowed
//! to overlap, and every region clips its own content.

use super::UiState;
use crate::runtime::ShowRuntime;
use crate::theme;
use egui::{Align, Button, Color32, Layout, Rect, RichText, Ui, UiBuilder};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::ShowTime;

pub const HEIGHT: f32 = 104.0;
const GAP: f32 = 10.0;

/// Runs `f` inside `rect`, clipped to it.
fn region(ui: &mut Ui, rect: Rect, f: impl FnOnce(&mut Ui)) {
    ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)), |ui| {
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        f(ui);
    });
}

fn caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::TEXT_DIM).strong());
}

fn big_button(ui: &mut Ui, rect: Rect, label: &str, size: f32, fill: Color32, text: Color32) -> egui::Response {
    ui.put(rect, Button::new(RichText::new(label).size(size).strong().color(text)).fill(fill).corner_radius(6))
}

pub fn show(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState) {
    let full = ui.max_rect();
    let h = full.height();
    let cmd = |rt: &ShowRuntime, c: ShowCommand| rt.command(ControlSource::Ui, c);

    // Fixed regions; optional ones are dropped right-to-left when space runs out.
    const GO_W: f32 = 168.0;
    const STOP_W: f32 = 112.0;
    const PANIC_W: f32 = 140.0;
    const NEXT_MIN: f32 = 250.0;
    let mut optional = [("output", 176.0, false), ("clock", 196.0, false), ("running", 270.0, false)];
    let mut used = GO_W + STOP_W + PANIC_W + NEXT_MIN + 3.0 * GAP;
    for o in optional.iter_mut().rev() {
        if used + o.1 + GAP <= full.width() {
            used += o.1 + GAP;
            o.2 = true;
        }
    }
    let shown = |name: &str| optional.iter().any(|o| o.0 == name && o.2);
    let width_of = |name: &str| optional.iter().find(|o| o.0 == name).map_or(0.0, |o| o.1);

    let mut x = full.left();
    let mut take = |w: f32| {
        let r = Rect::from_min_size(egui::pos2(x, full.top()), egui::vec2(w, h));
        x += w + GAP;
        r
    };

    // GO
    let go_rect = take(GO_W);
    if big_button(ui, go_rect, "GO", 40.0, theme::go(), Color32::BLACK).on_hover_text("Space").clicked() {
        cmd(rt, ShowCommand::Go { list: None });
    }

    // NEXT (flexible)
    let next_w = full.width() - used + NEXT_MIN;
    let next_rect = take(next_w);
    region(ui, next_rect, |ui| {
        caption(ui, "NEXT UP");
        let (number, name) = match rt.bundle.project.active_list().and_then(|l| l.standby_cue()) {
            Some(c) => {
                use livestage_core::cue::Cue;
                (c.header().number.clone(), c.header().name.clone())
            }
            None => (String::new(), "End of list".into()),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&number).font(theme::clock_font(26.0)).color(theme::standby()));
            ui.add(egui::Label::new(RichText::new(name).size(24.0).strong().color(theme::standby())).truncate());
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Prev").on_hover_text("Up arrow").clicked() {
                cmd(rt, ShowCommand::StandbyPrevious);
            }
            if ui.button("Next").on_hover_text("Down arrow").clicked() {
                cmd(rt, ShowCommand::StandbyNext);
            }
            ui.separator();
            if ui.button("Pause all").on_hover_text("P").clicked() {
                cmd(rt, ShowCommand::PauseAll);
            }
            if ui.button("Resume").on_hover_text("R").clicked() {
                cmd(rt, ShowCommand::ResumeAll);
            }
        });
    });

    // STOP ALL / PANIC
    let stop_rect = take(STOP_W);
    if big_button(ui, stop_rect, "STOP\nALL", 18.0, Color32::from_rgb(0x4A, 0x18, 0x17), theme::TEXT)
        .on_hover_text("Stop every cue with a 1 s fade")
        .clicked()
    {
        cmd(rt, ShowCommand::StopAll { fade: Some(ShowTime::from_secs(1)) });
    }
    let panic_rect = take(PANIC_W);
    if big_button(ui, panic_rect, "PANIC", 28.0, theme::stop(), Color32::WHITE)
        .on_hover_text("Esc. Press twice within 1.5 s for a hard cut + DMX blackout")
        .clicked()
    {
        cmd(rt, ShowCommand::Panic { hard: false });
    }

    // RUNNING: large countdown of the latest running cue
    if shown("running") {
        let r = take(width_of("running"));
        ui.painter().rect_filled(r, 6.0, theme::PANEL_RAISED);
        region(ui, r.shrink(8.0), |ui| match rt.running_summary() {
            Some((number, name, elapsed, remaining)) => {
                caption(ui, &format!("RUNNING · {} ACTIVE", rt.running_count()));
                let (big, label) = match remaining {
                    Some(rem) => (format!("-{}", &rem.to_clock_string()[3..]), "remaining"),
                    None => (elapsed.to_clock_string()[3..].to_string(), "elapsed"),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(big).font(theme::clock_font(32.0)).color(theme::active()));
                    ui.label(RichText::new(label).size(11.0).color(theme::TEXT_DIM));
                });
                ui.add(egui::Label::new(RichText::new(format!("{number}  {name}")).color(theme::TEXT)).truncate());
            }
            None => {
                caption(ui, "RUNNING");
                ui.label(RichText::new("--:--.--").font(theme::clock_font(32.0)).color(theme::TEXT_DIM));
                ui.label(RichText::new("No cues running").color(theme::TEXT_DIM));
            }
        });
    }

    // CLOCK
    if shown("clock") {
        let r = take(width_of("clock"));
        ui.painter().rect_filled(r, 6.0, theme::PANEL_RAISED);
        region(ui, r.shrink(8.0), |ui| {
            caption(ui, "TIME OF DAY");
            let now = chrono::Local::now();
            ui.label(RichText::new(now.format("%H:%M:%S").to_string()).font(theme::clock_font(34.0)).color(theme::TEXT));
            ui.label(RichText::new(now.format("%a %d %b").to_string()).color(theme::TEXT_DIM));
        });
    }

    // OUTPUTS + MASTER
    if shown("output") {
        let r = take(width_of("output"));
        region(ui, r, |ui| outputs_and_master(ui, rt, st));
    }
}

fn outputs_and_master(ui: &mut Ui, rt: &mut ShowRuntime, _st: &mut UiState) {
    let live = rt.outputs_live;
    let (txt, fill, color) = if live {
        ("OUTPUTS LIVE", theme::stop(), Color32::WHITE)
    } else {
        ("Outputs off", Color32::from_rgb(0x2A, 0x2A, 0x2A), theme::TEXT)
    };
    let w = ui.available_width();
    if ui
        .add_sized([w, 44.0], Button::new(RichText::new(txt).strong().size(15.0).color(color)).fill(fill).corner_radius(6))
        .on_hover_text("Open / close program windows (kiosk fullscreen on mapped screens)")
        .clicked()
    {
        rt.command(ControlSource::Ui, ShowCommand::SetOutputsFullscreen { enabled: !live });
    }
    ui.add_space(6.0);
    caption(ui, "MASTER");
    let mut db = rt.bundle.project.audio.master_gain_db;
    ui.spacing_mut().slider_width = w - 64.0;
    if ui.add(egui::Slider::new(&mut db, -60.0..=6.0).suffix(" dB").fixed_decimals(1)).changed() {
        rt.command(ControlSource::Ui, ShowCommand::SetMasterGain { db });
    }
}

/// File / Edit / View / Show menus.
pub fn menu_bar(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState) {
    egui::menu::bar(ui, |ui| {
        ui.menu_button("File", |ui| {
            if ui.add(Button::new("Save").shortcut_text("Cmd+S")).clicked() {
                rt.save();
                ui.close_menu();
            }
            if ui.button("Import media…").clicked() {
                st.import_open = true;
                ui.close_menu();
            }
        });
        ui.menu_button("Edit", |ui| {
            if ui.add_enabled(rt.undo.can_undo(), Button::new("Undo").shortcut_text("Cmd+Z")).clicked() {
                rt.undo();
                ui.close_menu();
            }
            if ui.add_enabled(rt.undo.can_redo(), Button::new("Redo").shortcut_text("Shift+Cmd+Z")).clicked() {
                rt.redo();
                ui.close_menu();
            }
        });
        ui.menu_button("View", |ui| {
            for tab in super::Tab::ALL {
                if ui.button(tab.title()).clicked() {
                    st.focus_tab(tab);
                    ui.close_menu();
                }
            }
            ui.separator();
            if ui.button("Reset layout").clicked() {
                st.reset_layout();
                ui.close_menu();
            }
        });
        ui.menu_button("Show", |ui| {
            let live = rt.outputs_live;
            if ui.button(if live { "Close program outputs" } else { "Open program outputs" }).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::SetOutputsFullscreen { enabled: !live });
                ui.close_menu();
            }
            if ui.add(Button::new("Stop all").shortcut_text("")).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::StopAll { fade: Some(ShowTime::from_secs(1)) });
                ui.close_menu();
            }
            if ui.add(Button::new("Panic").shortcut_text("Esc")).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::Panic { hard: false });
                ui.close_menu();
            }
        });
        ui.separator();
        let title = format!("{}{}", rt.bundle.project.name, if rt.dirty { "  •  unsaved" } else { "" });
        ui.label(RichText::new(title).color(if rt.dirty { theme::standby() } else { theme::TEXT_DIM }))
            .on_hover_text(rt.bundle.root().display().to_string());
    });
}

/// Import dialog (File ▸ Import media…). Files can also be dropped on the window.
pub fn import_window(ctx: &egui::Context, rt: &mut ShowRuntime, st: &mut UiState) {
    if !st.import_open {
        return;
    }
    let mut open = true;
    egui::Window::new("Import media").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.label("Path of an audio, video or image file. It is copied into the show's assets/ folder and a cue is created.");
        ui.label(RichText::new("Tip: you can also drop files onto the LiveStage window.").color(theme::TEXT_DIM));
        ui.add(egui::TextEdit::singleline(&mut st.import_path).hint_text("/path/to/file.wav").desired_width(420.0));
        ui.horizontal(|ui| {
            let ok = !st.import_path.trim().is_empty();
            if ui.add_enabled(ok, Button::new("Import")).clicked() {
                let p = std::path::PathBuf::from(st.import_path.trim());
                rt.import_and_create_cue(&p);
                st.import_path.clear();
                st.import_open = false;
            }
            if ui.button("Cancel").clicked() {
                st.import_open = false;
            }
        });
    });
    if !open {
        st.import_open = false;
    }
}

/// Samples show-thread counters once per second.
fn show_health(rt: &ShowRuntime, st: &mut UiState) {
    use crate::runtime::ShowStats;
    use std::sync::atomic::Ordering;
    let now = std::time::Instant::now();
    let ticks = rt.show_stats.ticks.load(Ordering::Relaxed);
    match st.health.0 {
        Some(at) if now.duration_since(at).as_secs_f32() < 1.0 => {}
        Some(at) => {
            let hz = (ticks - st.health.1) as f32 / now.duration_since(at).as_secs_f32();
            let late = ShowStats::take(&rt.show_stats.max_late_us) as f32 / 1000.0;
            let wait = ShowStats::take(&rt.show_stats.max_wait_us) as f32 / 1000.0;
            let tick = ShowStats::take(&rt.show_stats.max_tick_us) as f32 / 1000.0;
            st.health = (Some(now), ticks, format!("Show {hz:.0} Hz · late ≤{late:.1} ms · lock ≤{wait:.1} ms · tick ≤{tick:.2} ms"));
        }
        None => st.health = (Some(now), ticks, "Show thread starting…".into()),
    }
}

pub fn status_bar(ui: &mut Ui, rt: &ShowRuntime, st: &mut UiState) {
    show_health(rt, st);
    ui.horizontal_centered(|ui| {
        let dim = theme::TEXT_DIM;
        match &rt.audio {
            Some(a) => {
                let load = a.dsp_load();
                let c = if load > 0.7 { theme::stop() } else { dim };
                ui.label(RichText::new(format!("Audio: {} · {} Hz · {} ch · DSP {:.0}%", a.device_name(), a.sample_rate(), a.channels(), load * 100.0)).color(c));
                let leaked = a.shared().leaked.load(std::sync::atomic::Ordering::Relaxed);
                if leaked > 0 {
                    ui.colored_label(theme::standby(), format!("RT queue overflow ×{leaked}"));
                }
            }
            None => {
                ui.colored_label(theme::stop(), "Audio offline");
            }
        }
        ui.separator();
        let sched = rt.show_stats.scheduling.lock().clone();
        ui.label(RichText::new(&st.health.2).color(dim)).on_hover_text(format!("Scheduling: {sched}\nWorst values over the last second"));
        ui.separator();
        ui.label(RichText::new(format!("DMX {} pkts", rt.dmx.packets_sent())).color(dim));
        ui.separator();
        let osc = rt.osc_port().map_or("OSC off".to_string(), |p| format!("OSC :{p}"));
        let ws = rt.ws_port().map_or("WS off".to_string(), |p| format!("WS :{p}"));
        ui.label(RichText::new(format!("{osc} · {ws}")).color(dim));
        if let Some(last) = rt.log.back() {
            ui.separator();
            let c = match last.level {
                crate::runtime::LogLevel::Error => theme::stop(),
                crate::runtime::LogLevel::Warn => theme::standby(),
                crate::runtime::LogLevel::Info => dim,
            };
            ui.add(egui::Label::new(RichText::new(&last.text).color(c)).truncate());
        }
    });
}
