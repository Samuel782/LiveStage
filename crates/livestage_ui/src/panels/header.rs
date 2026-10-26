//! Fixed header: the top control deck.
//!
//! ```text
//! ┌ icon  Show title ─────────┐ ┌ 01:00:12:04 ┐ ┌ health ┐ ┌ master ┐ DBO  STOP  PANIC  ▲▼  ┌──── GO ────┐
//! │ File Edit View Show ↶ ↷   │ │ INTERNAL ●  │ │ pills  │ │ ▬▬▬▬ ▮ │                     │ 3  Preset  │
//! └ Edit │ Live ──────────────┘ └ source  ▾  ┘ └────────┘ └────────┘                     └────────────┘
//! ```
//!
//! Regions have fixed widths computed from the window width; optional
//! modules (health, master, blackout) drop out right-to-left on narrow
//! windows instead of overlapping, and every region clips its content.

use super::widgets::clock::smpte_clock;
use super::widgets::go_button::{go_button, Standby};
use super::widgets::inputs::segmented;
use super::widgets::meter::{vu_meter, Orientation};
use super::widgets::{caption_text, key, KeyTone};
use super::UiState;
use crate::theme;
use crate::view::UiRuntime;
use egui::{Align, Align2, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::timecode::TimecodeSource;
use livestage_core::ShowTime;

pub const HEIGHT: f32 = 100.0;
const GAP: f32 = theme::GRID;

/// Runs `f` inside `rect`, clipped to it.
fn region(ui: &mut Ui, rect: Rect, layout: Layout, f: impl FnOnce(&mut Ui)) {
    ui.scope_builder(UiBuilder::new().max_rect(rect).layout(layout), |ui| {
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        f(ui);
    });
}

pub fn show(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    super::transport::sample_health(rt, st);
    let full = ui.max_rect();
    let h = full.height();

    const LEFT_W: f32 = 300.0;
    const CLOCK_W: f32 = 280.0;
    const STOP_W: f32 = 80.0;
    const PANIC_W: f32 = 96.0;
    const STEP_W: f32 = 34.0;
    const GO_MIN: f32 = 190.0;
    const GO_MAX: f32 = 260.0;
    // Optional modules, dropped first → last.
    let mut optional = [("health", 228.0, false), ("master", 180.0, false), ("dbo", 60.0, false)];
    let mut used = LEFT_W + CLOCK_W + STOP_W + PANIC_W + STEP_W + GO_MIN + 6.0 * GAP;
    for o in optional.iter_mut().rev() {
        if used + o.1 + GAP <= full.width() {
            used += o.1 + GAP;
            o.2 = true;
        }
    }
    let shown = |name: &str| optional.iter().any(|o| o.0 == name && o.2);
    let width_of = |name: &str| optional.iter().find(|o| o.0 == name).map_or(0.0, |o| o.1);
    st.deck_shows_system = shown("health");
    let go_w = (GO_MIN + (full.width() - used)).clamp(GO_MIN, GO_MAX);
    // Spare width (beyond the GO's growth) goes to the left of the clock.
    let spare = (full.width() - used - (go_w - GO_MIN)).max(0.0);

    // ---- left: identity and editing
    let left = Rect::from_min_size(full.min, Vec2::new(LEFT_W + spare * 0.5, h));
    region(ui, left, Layout::top_down(Align::Min), |ui| identity(ui, rt, st));

    // ---- centre: timecode
    let clock = Rect::from_min_size(Pos2::new(left.right() + GAP, full.top()), Vec2::new(CLOCK_W, h));
    timecode(ui, clock, rt, st);

    // ---- right, laid out from the right edge
    let mut x = full.right();
    let mut take = |w: f32| {
        x -= w;
        let r = Rect::from_min_size(Pos2::new(x, full.top()), Vec2::new(w, h));
        x -= GAP;
        r
    };
    let go_rect = take(go_w);
    let standby = rt.bundle.project.active_list().and_then(|l| l.standby_cue()).map(|c| {
        use livestage_core::cue::Cue;
        (c.header().number.clone(), c.header().name.clone())
    });
    let sb = standby.as_ref().map(|(n, name)| Standby { number: n, name });
    if go_button(ui, go_rect, sb).clicked() {
        rt.command(ControlSource::Ui, ShowCommand::Go { list: None });
    }
    let step = take(STEP_W);
    standby_steps(ui, step, rt);
    let panic = take(PANIC_W);
    if key(ui, panic, "PANIC", 22.0, Some("ESC"), KeyTone::Danger, false)
        .on_hover_text("Esc. Press twice within 1.5 s for a hard cut + DMX blackout")
        .clicked()
    {
        rt.command(ControlSource::Ui, ShowCommand::Panic { hard: false });
    }
    let stop = take(STOP_W);
    if key(ui, stop, "STOP\nALL", 15.0, Some("1 s"), KeyTone::Warn, false).on_hover_text("Stop every cue with a 1 s fade").clicked() {
        rt.command(ControlSource::Ui, ShowCommand::StopAll { fade: Some(ShowTime::from_secs(1)) });
    }
    if shown("dbo") {
        let r = take(width_of("dbo"));
        blackout(ui, r, rt);
    }
    if shown("master") {
        let r = take(width_of("master"));
        master(ui, r, rt, st);
    }
    if shown("health") {
        let r = take(width_of("health"));
        health(ui, r, rt, st);
    }
}

/// Icon, show title, menus, undo / redo and the Edit / Live switch.
fn identity(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::GRID;
        crate::assets::icon_image(ui, 30.0).on_hover_text("LiveStage");
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let title = ui.add(egui::Label::new(RichText::new(&rt.bundle.project.name).size(theme::type_scale::SUBHEAD).strong()).truncate());
            title.on_hover_text(rt.bundle.root().display().to_string());
            let (status, c) = if rt.dirty { ("Edited — not saved", theme::standby()) } else { ("Saved", theme::text_faint()) };
            let mut line = status.to_string();
            for job in &rt.imports {
                use std::sync::atomic::Ordering;
                let (done, total) = (job.progress.0.load(Ordering::Relaxed), job.progress.1.load(Ordering::Relaxed));
                line = if total > 0 { format!("Rendering {} {done}/{total}", job.name) } else { format!("Rendering {}", job.name) };
            }
            ui.label(RichText::new(line).size(theme::type_scale::SMALL).color(c));
        });
    });
    ui.spacing_mut().item_spacing.y = 4.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        super::transport::menus(ui, rt, st);
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if ui.add_enabled(rt.undo.can_undo(), egui::Button::new(RichText::new("↶").size(16.0)).min_size(Vec2::new(30.0, 24.0))).on_hover_text(format!("Undo ({})", super::widgets::shortcut("Cmd+Z"))).clicked() {
            rt.undo();
        }
        if ui.add_enabled(rt.undo.can_redo(), egui::Button::new(RichText::new("↷").size(16.0)).min_size(Vec2::new(30.0, 24.0))).on_hover_text(format!("Redo ({})", super::widgets::shortcut("Cmd+Shift+Z"))).clicked() {
            rt.redo();
        }
        ui.add_space(theme::GRID);
        let mut live = st.live_mode;
        if segmented(ui, &mut live, &[(false, "Edit"), (true, "Live")]) {
            st.live_mode = live;
        }
        ui.add_space(theme::GRID);
        let on_air = rt.outputs_live;
        let (label, ink, fill) = if on_air { ("● Outputs live", Color32::WHITE, theme::stop()) } else { ("Outputs off", theme::text(), theme::control()) };
        if ui
            .add(egui::Button::new(RichText::new(label).color(ink)).fill(fill).min_size(Vec2::new(0.0, 24.0)))
            .on_hover_text("Open / close the program windows (kiosk fullscreen on mapped screens)")
            .clicked()
        {
            rt.command(ControlSource::Ui, ShowCommand::SetOutputsFullscreen { enabled: !on_air });
        }
    });
}

/// The SMPTE clock with the source selector and transport.
fn timecode(ui: &mut Ui, rect: Rect, rt: &mut UiRuntime, st: &mut UiState) {
    let p = ui.painter();
    p.rect(rect, theme::RADIUS as f32 + 2.0, theme::recessed(), theme::hairline(), StrokeKind::Inside);
    let inner = rect.shrink2(Vec2::new(theme::GRID + 4.0, 6.0));
    region(ui, inner, Layout::top_down(Align::Min), |ui| {
        let t = rt.timecode.transport();
        let source = rt.timecode.config().source;
        ui.spacing_mut().item_spacing.y = 4.0;
        if smpte_clock(ui, &t, source, 28.0).on_hover_text("Timecode settings").clicked() {
            st.settings_open = true;
            st.settings_section = super::settings::Section::Timecode;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let mut chosen = source;
            egui::ComboBox::from_id_salt("hdr-tc-source").width(112.0).selected_text(super::widgets::clock::source_label(source)).show_ui(ui, |ui| {
                for s in TimecodeSource::ALL {
                    ui.selectable_value(&mut chosen, s, s.label());
                }
            });
            if chosen != source {
                rt.bundle.project.timecode.source = chosen;
                rt.apply_timecode_config();
                rt.mark_changed();
            }
            if source == TimecodeSource::Internal {
                let (label, c) = if t.running { ("Stop", ShowCommand::TimecodeStop) } else { ("Start", ShowCommand::TimecodeStart) };
                if ui.small_button(label).clicked() {
                    rt.command(ControlSource::Ui, c);
                }
                if ui.small_button("Reset").on_hover_text("Stop and park at the start timecode").clicked() {
                    rt.timecode.reset();
                }
            } else if t.state != livestage_core::timecode::LockState::Stopped {
                ui.label(RichText::new(format!("{:+.0} ppm", t.drift_ppm())).font(theme::clock_font(11.0)).color(theme::text_dim()));
            }
        });
    });
}

/// ▲ / ▼: move the standby cue.
fn standby_steps(ui: &mut Ui, rect: Rect, rt: &UiRuntime) {
    let half = (rect.height() - 4.0) / 2.0;
    for (i, (glyph, cmd, tip)) in [("▲", ShowCommand::StandbyPrevious, "Previous cue (Up arrow)"), ("▼", ShowCommand::StandbyNext, "Next cue (Down arrow)")].into_iter().enumerate() {
        let r = Rect::from_min_size(Pos2::new(rect.left(), rect.top() + i as f32 * (half + 4.0)), Vec2::new(rect.width(), half));
        let resp = ui.interact(r, ui.id().with(("standby-step", i)), Sense::click());
        let fill = if resp.is_pointer_button_down_on() {
            theme::control_active()
        } else if resp.hovered() {
            theme::control_hover()
        } else {
            theme::control()
        };
        ui.painter().rect(r, theme::RADIUS as f32, fill, Stroke::new(theme::HAIRLINE, theme::border_hi()), StrokeKind::Inside);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(12.0), theme::text());
        if resp.on_hover_text(tip).clicked() {
            rt.command(ControlSource::Ui, cmd);
        }
    }
}

/// Desk blackout toggle (intensities to zero; positions and colours hold).
fn blackout(ui: &mut Ui, rect: Rect, rt: &mut UiRuntime) {
    let on = rt.dmx.console.blackout;
    let tone = if on { KeyTone::Danger } else { KeyTone::Neutral };
    let r = key(ui, rect, "DBO", 15.0, Some(if on { "ON" } else { "off" }), tone, on);
    if r.on_hover_text("Master blackout: every intensity to zero (positions and colours hold)").clicked() {
        rt.dmx.console.blackout = !on;
    }
}

/// Master volume with the master meter.
fn master(ui: &mut Ui, rect: Rect, rt: &mut UiRuntime, st: &mut UiState) {
    let p = ui.painter();
    p.rect(rect, theme::RADIUS as f32 + 2.0, theme::recessed(), theme::hairline(), StrokeKind::Inside);
    let inner = rect.shrink2(Vec2::new(theme::GRID + 2.0, theme::GRID));
    region(ui, inner, Layout::top_down(Align::Min), |ui| {
        let mut db = rt.bundle.project.audio.master_gain_db;
        ui.horizontal(|ui| {
            ui.label(caption_text("Master"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{db:+.1} dB")).font(theme::clock_font(12.0)).color(theme::text()));
            });
        });
        ui.spacing_mut().slider_width = ui.available_width();
        if ui.add(egui::Slider::new(&mut db, -60.0..=6.0).show_value(false)).on_hover_text("Master volume (all outputs)").changed() {
            rt.command(ControlSource::Ui, ShowCommand::SetMasterGain { db });
        }
        ui.add_space(2.0);
        let active = rt.audio.is_some();
        vu_meter(ui, Vec2::new(ui.available_width(), 10.0), &mut st.master_meter, Orientation::Horizontal, active);
    });
}

/// Health pills: show thread, audio, watchdog, CPU and output rendering.
fn health(ui: &mut Ui, rect: Rect, rt: &UiRuntime, st: &UiState) {
    let p = ui.painter();
    p.rect(rect, theme::RADIUS as f32 + 2.0, theme::recessed(), theme::hairline(), StrokeKind::Inside);
    let hlt = &st.health;
    let measured = hlt.hz > 0.0;
    let ok = theme::active();
    let warn = theme::standby();
    let bad = theme::stop_text();
    let w = &st.watch;
    let audio = match &rt.audio {
        None => ("off".to_string(), bad),
        Some(_) if w.audio_stalled => ("stalled".to_string(), bad),
        Some(a) => {
            let load = a.dsp_load();
            let c = if load > 0.7 || a.leaked() > 0 || a.underruns() > 0 { warn } else { ok };
            (format!("{:.0} %", load * 100.0), c)
        }
    };
    let pills: [(&str, String, Color32); 6] = [
        ("SHOW", if measured { format!("{:.0} Hz", hlt.hz) } else { "—".into() }, if !measured { theme::text_faint() } else if hlt.hz > 900.0 && hlt.late_ms < 2.0 && !w.show_stalled { ok } else { warn }),
        ("AUDIO", audio.0, audio.1),
        ("WATCH", if w.alarm() { "alarm".into() } else if w.notable() { format!("{} ev", w.stalls + w.faults) } else { "ok".into() }, if w.alarm() { bad } else if w.notable() { warn } else { ok }),
        ("CPU", hlt.cpu_pct.map_or("—".into(), |c| format!("{c:.0} %")), if hlt.cpu_pct.is_some_and(|c| c > 150.0) { warn } else { ok }),
        ("GPU", format!("{:.0} fps", st.output_fps), if w.output_stalled { bad } else { ok }),
        ("LATE", if measured { format!("{:.1} ms", hlt.late_ms) } else { "—".into() }, if hlt.late_ms < 2.0 { ok } else { warn }),
    ];
    let inner = rect.shrink(6.0);
    let (cols, rows) = (3, 2);
    let cw = (inner.width() - (cols - 1) as f32 * 4.0) / cols as f32;
    let ch = (inner.height() - (rows - 1) as f32 * 4.0) / rows as f32;
    for (i, (label, value, c)) in pills.iter().enumerate() {
        let r = Rect::from_min_size(inner.min + Vec2::new((i % cols) as f32 * (cw + 4.0), (i / cols) as f32 * (ch + 4.0)), Vec2::new(cw, ch));
        p.rect(r, theme::RADIUS as f32, theme::wash(*c, 0.10), Stroke::new(theme::HAIRLINE, theme::wash(*c, 0.35)), StrokeKind::Inside);
        p.circle_filled(Pos2::new(r.left() + 8.0, r.top() + 10.0), 3.0, *c);
        p.text(Pos2::new(r.left() + 15.0, r.top() + 10.0), Align2::LEFT_CENTER, *label, FontId::proportional(9.0), theme::text_dim());
        p.text(Pos2::new(r.left() + 8.0, r.bottom() - 6.0), Align2::LEFT_BOTTOM, value, theme::clock_font(12.5), theme::text());
    }
    let sched = rt.show_stats.scheduling.lock().clone();
    let av = &st.av_sync;
    let av_line = if av.layers == 0 {
        "A/V lock: no timeline video playing".to_string()
    } else {
        format!("A/V lock: {} layer(s) on the audio clock, worst offset {:.1} ms, {} re-time(s)", av.layers, av.worst_ms, av.snaps)
    };
    let cpu = match hlt.cpu_pct {
        Some(c) => format!("CPU: {c:.0} % of one core (whole process, last second)"),
        None => "CPU: not measured on this platform".into(),
    };
    ui.allocate_rect(rect, Sense::hover()).on_hover_text(format!(
        "Show thread: {:.0} Hz, worst lateness {:.1} ms, lock wait {:.1} ms, tick {:.2} ms (last second)\nScheduling: {sched}\n{cpu}\nGPU: program outputs render at {:.0} fps (wgpu reports no GPU load)\n{av_line}\n{}",
        hlt.hz,
        hlt.late_ms,
        hlt.lock_ms,
        hlt.tick_ms,
        st.output_fps,
        super::transport::watch_detail(&st.watch)
    ));
}
