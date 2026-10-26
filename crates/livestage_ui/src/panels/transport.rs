//! Command deck (GO / standby / STOP / PANIC keys and telemetry modules),
//! menu bar and status bar.
//!
//! The deck is laid out in fixed regions computed from the available
//! width. Optional modules are dropped on narrow windows rather than
//! allowed to overlap, and every region clips its own content.

use super::widgets::{caption_text, key, module, small_key, KeyTone};
use super::UiState;
use crate::theme;
use crate::view::UiRuntime;
use egui::{Align, Button, Layout, Rect, RichText, Ui, UiBuilder};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::ShowTime;

pub const HEIGHT: f32 = 112.0;
const GAP: f32 = 8.0;

/// Runs `f` inside `rect`, clipped to it.
fn region(ui: &mut Ui, rect: Rect, f: impl FnOnce(&mut Ui)) {
    ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)), |ui| {
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        f(ui);
    });
}

/// Show-thread health, sampled once a second.
#[derive(Default)]
pub struct Health {
    sampled: Option<std::time::Instant>,
    ticks: u64,
    pub hz: f32,
    pub late_ms: f32,
    pub lock_ms: f32,
    pub tick_ms: f32,
}

/// The command deck: GO, standby readout and transport keys, STOP / PANIC,
/// then telemetry modules (running cue, timecode, system health, clock,
/// outputs + master). Regions have fixed widths; optional modules drop
/// out right-to-left on narrow windows instead of overlapping.
pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    sample_health(rt, st);
    let full = ui.max_rect();
    let h = full.height();
    let cmd = |rt: &UiRuntime, c: ShowCommand| rt.command(ControlSource::Ui, c);

    const GO_W: f32 = 150.0;
    const STOP_W: f32 = 100.0;
    const PANIC_W: f32 = 124.0;
    const NEXT_MIN: f32 = 280.0;
    // Dropped first → last when narrow.
    let mut optional = [("clock", 160.0, false), ("system", 196.0, false), ("timecode", 212.0, false), ("output", 176.0, false), ("running", 252.0, false)];
    let mut used = GO_W + STOP_W + PANIC_W + NEXT_MIN + 3.0 * GAP;
    for o in optional.iter_mut().rev() {
        if used + o.1 + GAP <= full.width() {
            used += o.1 + GAP;
            o.2 = true;
        }
    }
    let shown = |name: &str| optional.iter().any(|o| o.0 == name && o.2);
    let width_of = |name: &str| optional.iter().find(|o| o.0 == name).map_or(0.0, |o| o.1);
    st.deck_shows_system = shown("system");

    let mut x = full.left();
    let mut take = |w: f32| {
        let r = Rect::from_min_size(egui::pos2(x, full.top()), egui::vec2(w, h));
        x += w + GAP;
        r
    };

    // GO
    let go_rect = take(GO_W);
    let standby = rt.bundle.project.active_list().and_then(|l| l.standby_cue()).is_some();
    if key(ui, go_rect, "GO", 38.0, Some("SPACE"), KeyTone::Go, standby).on_hover_text("Fire the standby cue (Space)").clicked() {
        cmd(rt, ShowCommand::Go { list: None });
    }

    // NEXT (flexible)
    let next_w = full.width() - used + NEXT_MIN;
    let next_rect = take(next_w);
    let status = if standby { Some(("standby", theme::standby())) } else { Some(("end of list", theme::TEXT_FAINT)) };
    let inner = module(ui, next_rect, "Next up", status);
    region(ui, inner, |ui| {
        let (number, name) = match rt.bundle.project.active_list().and_then(|l| l.standby_cue()) {
            Some(c) => {
                use livestage_core::cue::Cue;
                (c.header().number.clone(), c.header().name.clone())
            }
            None => ("—".into(), "Nothing on standby".into()),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&number).font(theme::clock_font(26.0)).color(theme::standby()));
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(name).size(22.0).color(if standby { theme::TEXT } else { theme::TEXT_DIM })).truncate());
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if small_key(ui, "◂ Prev").on_hover_text("Up arrow").clicked() {
                cmd(rt, ShowCommand::StandbyPrevious);
            }
            if small_key(ui, "Next ▸").on_hover_text("Down arrow").clicked() {
                cmd(rt, ShowCommand::StandbyNext);
            }
            ui.add_space(6.0);
            if small_key(ui, "Pause all").on_hover_text("P").clicked() {
                cmd(rt, ShowCommand::PauseAll);
            }
            if small_key(ui, "Resume").on_hover_text("R").clicked() {
                cmd(rt, ShowCommand::ResumeAll);
            }
        });
    });

    // STOP ALL / PANIC
    let stop_rect = take(STOP_W);
    if key(ui, stop_rect, "STOP\nALL", 17.0, Some("1 s FADE"), KeyTone::Warn, false).on_hover_text("Stop every cue with a 1 s fade").clicked() {
        cmd(rt, ShowCommand::StopAll { fade: Some(ShowTime::from_secs(1)) });
    }
    let panic_rect = take(PANIC_W);
    if key(ui, panic_rect, "PANIC", 26.0, Some("ESC"), KeyTone::Danger, false)
        .on_hover_text("Esc. Press twice within 1.5 s for a hard cut + DMX blackout")
        .clicked()
    {
        cmd(rt, ShowCommand::Panic { hard: false });
    }

    // RUNNING: large countdown of the latest running cue
    if shown("running") {
        let r = take(width_of("running"));
        let n = rt.running_count();
        let status = (n > 0).then(|| (if n == 1 { "1 active".to_string() } else { format!("{n} active") }, theme::active()));
        let inner = module(ui, r, "Running", status.as_ref().map(|(t, c)| (t.as_str(), *c)));
        region(ui, inner, |ui| match rt.running_summary() {
            Some((number, name, elapsed, remaining)) => {
                let (big, label) = match remaining {
                    Some(rem) => (format!("-{}", &rem.to_clock_string()[3..]), "REM"),
                    None => (elapsed.to_clock_string()[3..].to_string(), "ELAPSED"),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(big).font(theme::clock_font(30.0)).color(theme::active()));
                    ui.label(caption_text(label));
                });
                ui.add(egui::Label::new(RichText::new(format!("{number}  {name}")).color(theme::TEXT_DIM)).truncate());
            }
            None => {
                ui.label(RichText::new("--:--.--").font(theme::clock_font(30.0)).color(theme::TEXT_FAINT));
                ui.label(RichText::new("No cues running").color(theme::TEXT_FAINT));
            }
        });
    }

    // TIMECODE
    if shown("timecode") {
        let r = take(width_of("timecode"));
        timecode(ui, r, rt, st);
    }

    // SYSTEM HEALTH
    if shown("system") {
        let r = take(width_of("system"));
        system(ui, r, rt, st);
    }

    // CLOCK
    if shown("clock") {
        let r = take(width_of("clock"));
        let inner = module(ui, r, "Time of day", None);
        region(ui, inner, |ui| {
            let now = chrono::Local::now();
            ui.label(RichText::new(now.format("%H:%M:%S").to_string()).font(theme::clock_font(24.0)).color(theme::TEXT));
            ui.label(caption_text(&now.format("%a %d %b").to_string()));
        });
    }

    // OUTPUTS + MASTER
    if shown("output") {
        let r = take(width_of("output"));
        outputs_and_master(ui, r, rt);
    }
}

fn timecode(ui: &mut Ui, rect: Rect, rt: &mut UiRuntime, st: &mut UiState) {
    use livestage_core::timecode::{LockState, TimecodeSource};
    let t = rt.timecode.transport();
    let source = rt.timecode.config().source;
    let (dot, state) = match t.state {
        LockState::Locked if t.running => (theme::active(), "locked"),
        LockState::Locked => (theme::active(), "ready"),
        LockState::Freewheel => (theme::standby(), "freewheel"),
        LockState::Locking => (theme::standby(), "locking"),
        LockState::Stopped => (theme::TEXT_FAINT, "stopped"),
    };
    let (dot, state) = if source == TimecodeSource::Internal {
        if t.running { (theme::active(), "running") } else { (theme::TEXT_FAINT, "stopped") }
    } else {
        (dot, state)
    };
    let title = format!("Timecode · {}", source.short());
    let inner = module(ui, rect, &title, Some((state, dot)));
    region(ui, inner, |ui| {
        let color = if t.running { theme::active() } else { theme::TEXT };
        let big = ui.add(egui::Label::new(RichText::new(t.timecode().to_string()).font(theme::clock_font(26.0)).color(color)).sense(egui::Sense::click()));
        if big.on_hover_text("Timecode settings").clicked() {
            st.settings_open = true;
            st.settings_section = super::settings::Section::Timecode;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if source == TimecodeSource::Internal {
                let (label, c) = if t.running { ("Stop", ShowCommand::TimecodeStop) } else { ("Start", ShowCommand::TimecodeStart) };
                if small_key(ui, label).clicked() {
                    rt.command(ControlSource::Ui, c);
                }
                if small_key(ui, "Reset").on_hover_text("Stop and park at the start timecode").clicked() {
                    rt.timecode.reset();
                }
                ui.label(caption_text(t.rate.token()));
            } else {
                let detail = match t.state {
                    LockState::Stopped => t.rate.label().to_string(),
                    _ => format!("{} · {:+.0} ppm", t.rate.token(), t.drift_ppm()),
                };
                ui.label(caption_text(&detail));
            }
        });
    });
}

/// Show thread rate and timing, audio DSP load and output frame rate.
fn system(ui: &mut Ui, rect: Rect, rt: &UiRuntime, st: &UiState) {
    let hlt = &st.health;
    let dsp = rt.audio.as_ref().map(|a| a.dsp_load());
    let ok = hlt.hz > 900.0 && hlt.late_ms < 2.0 && dsp.is_none_or(|d| d < 0.7);
    let measured = hlt.hz > 0.0;
    let status = if !measured { ("starting", theme::TEXT_FAINT) } else if ok { ("nominal", theme::active()) } else { ("check", theme::standby()) };
    let inner = module(ui, rect, "System", Some(status));
    let sched = rt.show_stats.scheduling.lock().clone();
    let cells = [
        ("SHOW", if measured { format!("{:.0} Hz", hlt.hz) } else { "—".into() }, hlt.hz > 900.0 || !measured),
        ("LATE", if measured { format!("{:.1} ms", hlt.late_ms) } else { "—".into() }, hlt.late_ms < 2.0),
        ("DSP", dsp.map_or("off".into(), |d| format!("{:.0} %", d * 100.0)), dsp.is_some_and(|d| d < 0.7)),
        ("OUT", format!("{:.0} fps", st.output_fps), st.av_sync.layers == 0 || st.av_sync.tight()),
    ];
    let p = ui.painter();
    let cw = inner.width() / 2.0;
    for (i, (label, value, good)) in cells.iter().enumerate() {
        let o = inner.min + egui::vec2((i % 2) as f32 * cw, (i / 2) as f32 * 36.0);
        p.text(o, egui::Align2::LEFT_TOP, *label, egui::FontId::proportional(9.5), theme::TEXT_FAINT);
        let c = if *good { theme::TEXT } else { theme::standby() };
        p.text(o + egui::vec2(0.0, 12.0), egui::Align2::LEFT_TOP, value, theme::clock_font(15.0), c);
    }
    let av = &st.av_sync;
    let av_line = if av.layers == 0 {
        "A/V lock: no timeline video playing".to_string()
    } else {
        format!("A/V lock: {} layer(s) on the audio clock, worst offset {:.1} ms, {} re-time(s)", av.layers, av.worst_ms, av.snaps)
    };
    ui.allocate_rect(rect, egui::Sense::hover()).on_hover_text(format!(
        "Show thread: {:.0} Hz, worst lateness {:.1} ms, lock wait {:.1} ms, tick {:.2} ms (last second)\nScheduling: {sched}\n{av_line}",
        hlt.hz, hlt.late_ms, hlt.lock_ms, hlt.tick_ms
    ));
}

fn outputs_and_master(ui: &mut Ui, rect: Rect, rt: &mut UiRuntime) {
    let live = rt.outputs_live;
    let key_rect = Rect::from_min_size(rect.min, egui::vec2(rect.width(), 52.0));
    let (label, tone) = if live { ("OUTPUTS LIVE", KeyTone::Danger) } else { ("OUTPUTS OFF", KeyTone::Neutral) };
    if key(ui, key_rect, label, 14.0, None, tone, live).on_hover_text("Open / close program windows (kiosk fullscreen on mapped screens)").clicked() {
        rt.command(ControlSource::Ui, ShowCommand::SetOutputsFullscreen { enabled: !live });
    }
    let below = Rect::from_min_max(egui::pos2(rect.left(), key_rect.bottom() + 8.0), rect.max);
    region(ui, below, |ui| {
        let mut db = rt.bundle.project.audio.master_gain_db;
        ui.horizontal(|ui| {
            ui.label(caption_text("Master"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{db:+.1} dB")).font(theme::clock_font(12.0)).color(theme::TEXT));
            });
        });
        ui.spacing_mut().slider_width = ui.available_width();
        if ui.add(egui::Slider::new(&mut db, -60.0..=6.0).show_value(false)).changed() {
            rt.command(ControlSource::Ui, ShowCommand::SetMasterGain { db });
        }
    });
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
            if menu_item(ui, if st.inspector_open { "Hide inspector" } else { "Show inspector" }, None).clicked() {
                st.inspector_open = !st.inspector_open;
            }
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
fn sample_health(rt: &UiRuntime, st: &mut UiState) {
    use crate::runtime::ShowStats;
    use std::sync::atomic::Ordering;
    let now = std::time::Instant::now();
    let ticks = rt.show_stats.ticks.load(Ordering::Relaxed);
    let h = &mut st.health;
    match h.sampled {
        Some(at) if now.duration_since(at).as_secs_f32() < 1.0 => {}
        Some(at) => {
            h.hz = (ticks - h.ticks) as f32 / now.duration_since(at).as_secs_f32();
            h.late_ms = ShowStats::take(&rt.show_stats.max_late_us) as f32 / 1000.0;
            h.lock_ms = ShowStats::take(&rt.show_stats.max_wait_us) as f32 / 1000.0;
            h.tick_ms = ShowStats::take(&rt.show_stats.max_tick_us) as f32 / 1000.0;
            h.sampled = Some(now);
            h.ticks = ticks;
        }
        None => {
            h.sampled = Some(now);
            h.ticks = ticks;
        }
    }
}

pub fn status_bar(ui: &mut Ui, rt: &UiRuntime, st: &mut UiState) {
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
        if !st.deck_shows_system {
            let h = &st.health;
            ui.label(RichText::new(format!("Show {:.0} Hz · late ≤{:.1} ms", h.hz, h.late_ms)).color(dim));
            ui.separator();
            ui.label(RichText::new(format!("Outputs {:.0} fps", st.output_fps)).color(dim));
            ui.separator();
        }
        if st.av_sync.layers > 0 {
            let av = st.av_sync;
            let text = RichText::new(format!("A/V lock ±{:.1} ms", av.worst_ms));
            let resp = if av.tight() { ui.label(text.color(dim)) } else { ui.colored_label(theme::standby(), text) };
            resp.on_hover_text(format!(
                "{} timeline video layer(s) frame-locked to their audio master.\nWorst offset over the last second; {} hard re-time(s) since outputs started.",
                av.layers, av.snaps
            ));
            ui.separator();
        }
        if !livestage_video::capabilities::movie_support().movies_available() {
            ui.colored_label(theme::stop(), "Movies off").on_hover_text(livestage_video::capabilities::movie_support().summary());
            ui.separator();
        }
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
