//! Application menus, the import dialog, and the health sampling shared by
//! the header and the status bar.

use super::UiState;
use crate::theme;
use crate::view::UiRuntime;
use egui::{Button, RichText, Ui};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::ShowTime;

/// Show-thread health, sampled once a second.
#[derive(Default)]
pub struct Health {
    sampled: Option<std::time::Instant>,
    ticks: u64,
    pub hz: f32,
    pub late_ms: f32,
    pub lock_ms: f32,
    pub tick_ms: f32,
    /// Process CPU time over the last second, % of one core (Unix only).
    pub cpu_pct: Option<f32>,
    cpu_secs: Option<f64>,
}

/// User + system CPU seconds used by this process so far.
fn process_cpu_secs() -> Option<f64> {
    #[cfg(unix)]
    {
        // SAFETY: getrusage writes the struct it is given.
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } != 0 {
            return None;
        }
        let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        Some(tv(u.ru_utime) + tv(u.ru_stime))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// LiveStage / File / Edit / View / Show menus (macOS-style rows with
/// shortcuts), laid out inline in the header.
pub fn menus(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    use super::widgets::{menu_item, menu_item_danger, menu_item_enabled, menu_separator, shortcut};
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("LiveStage", |ui| {
            ui.set_min_width(220.0);
            if menu_item(ui, "About LiveStage", None).clicked() {
                st.about_open = true;
            }
            menu_separator(ui);
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
            if menu_item(ui, if st.log_open { "Hide operator log" } else { "Show operator log" }, None).clicked() {
                st.log_open = !st.log_open;
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
        ui.label(RichText::new("Presentations are rendered to slides in the background (one Slides cue per document).").color(theme::text_dim()));
        ui.label(RichText::new("Tip: you can also drop files onto the LiveStage window.").color(theme::text_dim()));
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
pub fn sample_health(rt: &UiRuntime, st: &mut UiState) {
    use crate::runtime::ShowStats;
    use std::sync::atomic::Ordering;
    let now = std::time::Instant::now();
    let ticks = rt.show_stats.ticks.load(Ordering::Relaxed);
    let h = &mut st.health;
    match h.sampled {
        Some(at) if now.duration_since(at).as_secs_f32() < 1.0 => {}
        Some(at) => {
            let wall = now.duration_since(at).as_secs_f64();
            let cpu = process_cpu_secs();
            h.cpu_pct = match (cpu, h.cpu_secs) {
                (Some(c), Some(prev)) if wall > 0.0 => Some(((c - prev) / wall * 100.0) as f32),
                _ => None,
            };
            h.cpu_secs = cpu;
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
            h.cpu_secs = process_cpu_secs();
        }
    }
}

/// Watchdog findings for tooltips.
pub fn watch_detail(w: &super::WatchView) -> String {
    let mut lines = vec![format!(
        "Watchdog: {} stall(s) and {} contained fault(s) since launch (details in the log).",
        w.stalls, w.faults
    )];
    if w.show_stalled {
        lines.push("The show thread has stopped ticking: cues, DMX and control are frozen.".into());
    }
    if w.output_stalled {
        lines.push("The output thread has stopped rendering frames.".into());
    }
    if w.audio_stalled {
        lines.push("The audio device stopped calling back: check the interface, then Restart audio.".into());
    }
    if w.autosave_failures > 0 {
        lines.push(format!("Autosave failed {} time(s) in a row: check the disk, and save by hand.", w.autosave_failures));
    }
    lines.join("\n")
}
