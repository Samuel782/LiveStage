//! Transport bar (GO / NEXT / STOP / PANIC / countdown / clock / outputs),
//! menu bar and status bar.
//!
//! The transport is laid out in fixed regions computed from the available
//! width. Optional regions are dropped on narrow windows rather than allowed
//! to overlap, and every region clips its own content.

use super::UiState;
use crate::view::UiRuntime;
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

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    let full = ui.max_rect();
    let h = full.height();
    let cmd = |rt: &UiRuntime, c: ShowCommand| rt.command(ControlSource::Ui, c);

    // Fixed regions; optional ones are dropped right-to-left when space runs out.
    const GO_W: f32 = 168.0;
    const STOP_W: f32 = 112.0;
    const PANIC_W: f32 = 140.0;
    const NEXT_MIN: f32 = 250.0;
    // Dropped first → last when narrow: clock, timecode, outputs, running.
    let mut optional = [("clock", 196.0, false), ("timecode", 214.0, false), ("output", 176.0, false), ("running", 270.0, false)];
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

    // TIMECODE
    if shown("timecode") {
        let r = take(width_of("timecode"));
        ui.painter().rect_filled(r, 6.0, theme::PANEL_RAISED);
        region(ui, r.shrink(8.0), |ui| timecode(ui, rt, st));
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

fn timecode(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    use livestage_core::timecode::{LockState, TimecodeSource};
    let t = rt.timecode.transport();
    let source = rt.timecode.config().source;
    let (dot, state) = match t.state {
        LockState::Locked if t.running => (theme::active(), "LOCKED"),
        LockState::Locked => (theme::active(), "READY"),
        LockState::Freewheel => (theme::standby(), "FREEWHEEL"),
        LockState::Locking => (theme::standby(), "LOCKING"),
        LockState::Stopped => (theme::TEXT_DIM, "STOPPED"),
    };
    let state = if source == TimecodeSource::Internal { if t.running { "RUNNING" } else { "STOPPED" } } else { state };
    let head = ui
        .horizontal(|ui| {
            caption(ui, &format!("TIMECODE · {}", source.short()));
            ui.label(RichText::new("●").size(10.0).color(dot));
            ui.label(RichText::new(state).size(11.0).strong().color(dot));
        })
        .response;
    let color = if t.running { theme::active() } else { theme::TEXT };
    let big = ui.add(egui::Label::new(RichText::new(t.timecode().to_string()).font(theme::clock_font(30.0)).color(color)).sense(egui::Sense::click()));
    if head.union(big).on_hover_text("Timecode settings").clicked() {
        st.settings_open = true;
        st.settings_section = super::settings::Section::Timecode;
    }
    ui.horizontal(|ui| {
        if source == TimecodeSource::Internal {
            let (label, c) = if t.running { ("Stop", ShowCommand::TimecodeStop) } else { ("Start", ShowCommand::TimecodeStart) };
            if ui.small_button(label).clicked() {
                rt.command(ControlSource::Ui, c);
            }
            if ui.small_button("Reset").on_hover_text("Stop and park at the start timecode").clicked() {
                rt.timecode.reset();
            }
            ui.label(RichText::new(t.rate.token()).size(11.0).color(theme::TEXT_DIM));
        } else {
            let detail = match t.state {
                LockState::Stopped => t.rate.label().to_string(),
                _ => format!("{} · {:+.0} ppm", t.rate.token(), t.drift_ppm()),
            };
            ui.label(RichText::new(detail).size(11.0).color(theme::TEXT_DIM));
        }
    });
}

fn outputs_and_master(ui: &mut Ui, rt: &mut UiRuntime, _st: &mut UiState) {
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

/// LiveStage / File / Edit / View / Show menus (macOS-style rows with shortcuts).
pub fn menu_bar(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    use super::widgets::{menu_item, menu_item_danger, menu_item_enabled, menu_separator, shortcut};
    egui::menu::bar(ui, |ui| {
        ui.menu_button(RichText::new("LiveStage").strong(), |ui| {
            ui.set_min_width(220.0);
            if menu_item(ui, "Settings…", Some(&shortcut("Cmd+,"))).clicked() {
                st.settings_open = true;
            }
        });
        ui.menu_button("File", |ui| {
            ui.set_min_width(240.0);
            if menu_item(ui, "Save", Some(&shortcut("Cmd+S"))).clicked() {
                rt.save();
            }
            menu_separator(ui);
            if menu_item(ui, "Import media or presentation…", Some(&shortcut("Cmd+I"))).clicked() {
                st.import_open = true;
            }
        });
        ui.menu_button("Edit", |ui| {
            ui.set_min_width(240.0);
            if menu_item_enabled(ui, rt.undo.can_undo(), "Undo", Some(&shortcut("Cmd+Z"))).clicked() {
                rt.undo();
            }
            if menu_item_enabled(ui, rt.undo.can_redo(), "Redo", Some(&shortcut("Cmd+Shift+Z"))).clicked() {
                rt.redo();
            }
            menu_separator(ui);
            let sel = st.selected.filter(|id| rt.bundle.project.active_list().and_then(|l| l.index_of(*id)).is_some());
            if menu_item_enabled(ui, sel.is_some(), "Duplicate cue", Some(&shortcut("Cmd+D"))).clicked() {
                if let Some(id) = sel {
                    st.selected = rt.duplicate_cue(id).or(st.selected);
                }
            }
        });
        ui.menu_button("View", |ui| {
            ui.set_min_width(220.0);
            for tab in super::Tab::ALL {
                if menu_item(ui, tab.title(), None).clicked() {
                    st.focus_tab(tab);
                }
            }
            menu_separator(ui);
            if menu_item(ui, "Reset layout", None).clicked() {
                st.reset_layout();
            }
        });
        ui.menu_button("Show", |ui| {
            ui.set_min_width(240.0);
            let live = rt.outputs_live;
            if menu_item(ui, if live { "Close program outputs" } else { "Open program outputs" }, None).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::SetOutputsFullscreen { enabled: !live });
            }
            menu_separator(ui);
            if menu_item(ui, "Next slide", Some("→")).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::NextSlide);
            }
            if menu_item(ui, "Previous slide", Some("←")).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::PrevSlide);
            }
            menu_separator(ui);
            if menu_item(ui, "Pause all", Some("P")).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::PauseAll);
            }
            if menu_item(ui, "Resume all", Some("R")).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::ResumeAll);
            }
            if menu_item(ui, "Stop all (1 s fade)", None).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::StopAll { fade: Some(ShowTime::from_secs(1)) });
            }
            if menu_item_danger(ui, "Panic", Some("Esc")).clicked() {
                rt.command(ControlSource::Ui, ShowCommand::Panic { hard: false });
            }
        });
        ui.separator();
        let title = format!("{}{}", rt.bundle.project.name, if rt.dirty { "  •  unsaved" } else { "" });
        ui.label(RichText::new(title).color(if rt.dirty { theme::standby() } else { theme::TEXT_DIM }))
            .on_hover_text(rt.bundle.root().display().to_string());
        // Presentations still rendering.
        for job in &rt.imports {
            use std::sync::atomic::Ordering;
            let (done, total) = (job.progress.0.load(Ordering::Relaxed), job.progress.1.load(Ordering::Relaxed));
            ui.separator();
            ui.add(egui::Spinner::new().size(12.0));
            let p = if total > 0 { format!(" {done}/{total}") } else { String::new() };
            ui.label(RichText::new(format!("Rendering {}{p}", job.name)).color(theme::standby()));
        }
    });
}

/// Import dialog (File ▸ Import media…). Files can also be dropped on the window.
pub fn import_window(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
    if !st.import_open {
        return;
    }
    let mut open = true;
    if st.import_path.is_empty() {
        if let Some(dir) = &st.prefs.media_folder {
            st.import_path = format!("{}/", dir.display());
        }
    }
    egui::Window::new("Import media").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.label("Path of an audio, video, image, PDF or PowerPoint file. It is copied into the show's assets/ folder and a cue is created.");
        ui.label(RichText::new("Presentations are rendered to slides in the background (one Slides cue per document).").color(theme::TEXT_DIM));
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
fn show_health(rt: &UiRuntime, st: &mut UiState) {
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

pub fn status_bar(ui: &mut Ui, rt: &UiRuntime, st: &mut UiState) {
    show_health(rt, st);
    ui.horizontal_centered(|ui| {
        let dim = theme::TEXT_DIM;
        match &rt.audio {
            Some(a) => {
                let load = a.dsp_load();
                let c = if load > 0.7 { theme::stop() } else { dim };
                ui.label(RichText::new(format!("Audio: {} · {} Hz · {} ch · DSP {:.0}%", a.device_name(), a.sample_rate(), a.channels(), load * 100.0)).color(c));
                let leaked = a.leaked();
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
        if !livestage_video::capabilities::movie_support().movies_available() {
            ui.colored_label(theme::stop(), "Movies off").on_hover_text(livestage_video::capabilities::movie_support().summary());
            ui.separator();
        }
        ui.label(RichText::new(format!("Outputs {:.0} fps", st.output_fps)).color(dim))
            .on_hover_text("Output render thread (independent of this window): canvases, program windows, NDI");
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
