//! Fixed footer: protocol activity lights (OSC, WebSocket, MIDI, DMX out,
//! sACN in), the active cue count, audio and media status, and the
//! collapsible operator log.

use super::UiState;
use crate::runtime::LogLevel;
use crate::theme;
use crate::view::UiRuntime;
use egui::{Align, Color32, Layout, Pos2, RichText, Sense, Ui, Vec2};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub const HEIGHT: f32 = 28.0;
/// How long an activity light stays lit after traffic.
const BLINK: Duration = Duration::from_millis(250);

/// Traffic seen per channel, for the activity lights.
#[derive(Default)]
pub struct Activity {
    seen: [u64; 4],
    lit_until: [Option<Instant>; 4],
    /// DMX packets per second (last second).
    pub dmx_rate: f32,
    dmx_sampled: Option<(Instant, u64)>,
}

#[derive(Clone, Copy)]
enum Channel {
    Osc = 0,
    Ws = 1,
    Midi = 2,
    DmxOut = 3,
}

impl Activity {
    fn update(&mut self, rt: &UiRuntime) {
        let now = Instant::now();
        let s = &rt.show_stats;
        let dmx = rt.dmx.packets_sent();
        let counts = [s.osc_rx.load(Ordering::Relaxed), s.ws_rx.load(Ordering::Relaxed), s.midi_rx.load(Ordering::Relaxed), dmx];
        for (i, c) in counts.into_iter().enumerate() {
            if c != self.seen[i] {
                self.seen[i] = c;
                self.lit_until[i] = Some(now + BLINK);
            }
        }
        match self.dmx_sampled {
            Some((at, n)) if now.duration_since(at) >= Duration::from_secs(1) => {
                self.dmx_rate = dmx.saturating_sub(n) as f32 / now.duration_since(at).as_secs_f32();
                self.dmx_sampled = Some((now, dmx));
            }
            Some(_) => {}
            None => self.dmx_sampled = Some((now, dmx)),
        }
    }

    fn lit(&self, ch: Channel) -> bool {
        self.lit_until[ch as usize].is_some_and(|t| Instant::now() < t)
    }
}

/// An activity light with its label: bright while traffic flows, dim
/// when idle, hollow when the channel is off.
fn light(ui: &mut Ui, label: &str, enabled: bool, lit: bool, tip: String) {
    let font = egui::FontId::proportional(theme::type_scale::SMALL);
    let g = ui.fonts_mut(|f| f.layout_no_wrap(label.to_string(), font, theme::text_dim()));
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 14.0, HEIGHT - 6.0), Sense::hover());
    let p = ui.painter();
    let c = Pos2::new(rect.left() + 4.0, rect.center().y);
    if !enabled {
        p.circle_stroke(c, 3.5, egui::Stroke::new(1.0, theme::text_faint()));
    } else if lit {
        p.circle_filled(c, 6.0, theme::wash(theme::active(), 0.25));
        p.circle_filled(c, 3.5, theme::active());
    } else {
        p.circle_filled(c, 3.5, theme::wash(theme::active(), 0.35));
    }
    let text = if enabled { theme::text_dim() } else { theme::text_faint() };
    p.galley(Pos2::new(rect.left() + 12.0, rect.center().y - g.size().y / 2.0), g, text);
    resp.on_hover_text(tip);
}

fn separator(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, HEIGHT - 12.0), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), egui::Stroke::new(1.0, theme::border()));
}

fn level_color(level: LogLevel) -> Color32 {
    match level {
        LogLevel::Info => theme::text_dim(),
        LogLevel::Warn => theme::standby(),
        LogLevel::Error => theme::stop_text(),
    }
}

pub fn status_bar(ui: &mut Ui, rt: &UiRuntime, st: &mut UiState) {
    st.activity.update(rt);
    // Keep the lights blinking even when nothing else repaints.
    ui.ctx().request_repaint_after(Duration::from_millis(100));
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = theme::GRID;
        let a = &st.activity;
        let osc = rt.osc_port();
        let ws = rt.ws_port();
        light(ui, "OSC", osc.is_some(), a.lit(Channel::Osc), osc.map_or("OSC off".into(), |p| format!("OSC on UDP {p} · {} command(s) received", a.seen[0])));
        light(ui, "WS", ws.is_some(), a.lit(Channel::Ws), ws.map_or("WebSocket off".into(), |p| format!("WebSocket on TCP {p} · {} command(s) received", a.seen[1])));
        light(
            ui,
            "MIDI",
            rt.midi_in.is_some(),
            a.lit(Channel::Midi),
            rt.midi_in.as_ref().map_or("No MIDI input".into(), |m| format!("MIDI in: {} · {} command(s)", m.port_name, a.seen[2])),
        );
        let (sacn, artnet) = rt.bundle.project.dmx.universes.iter().fold((0, 0), |(s, n), u| match u.transport {
            livestage_core::dmx_config::DmxTransport::Sacn { .. } => (s + 1, n),
            livestage_core::dmx_config::DmxTransport::ArtNet { .. } => (s, n + 1),
            livestage_core::dmx_config::DmxTransport::Disabled => (s, n),
        });
        let dmx_label = match (sacn > 0, artnet > 0) {
            (true, true) => "sACN · Art-Net",
            (true, false) => "sACN",
            (false, true) => "Art-Net",
            (false, false) => "DMX",
        };
        light(
            ui,
            dmx_label,
            sacn + artnet > 0,
            a.lit(Channel::DmxOut),
            format!("DMX out: {sacn} sACN and {artnet} Art-Net universe(s) · {:.0} packets/s · {} sent", a.dmx_rate, rt.dmx.packets_sent()),
        );
        if rt.dmx.input_running {
            let sources: u32 = rt.dmx.input.iter().map(|(_, s, _)| *s as u32).sum();
            // Steady while sources are present (the receiver keeps no packet count).
            light(ui, "sACN in", true, sources > 0, format!("sACN input: {} universe(s), {sources} source(s)", rt.dmx.input.len()));
        }
        separator(ui);
        let n = rt.running_count();
        let (text, c) = match n {
            0 => ("No cues running".to_string(), theme::text_faint()),
            1 => ("1 cue running".to_string(), theme::active()),
            n => (format!("{n} cues running"), theme::active()),
        };
        ui.label(RichText::new(text).size(theme::type_scale::SMALL).color(c));
        separator(ui);
        match &rt.audio {
            Some(a) => {
                let c = if a.dsp_load() > 0.7 { theme::stop_text() } else { theme::text_dim() };
                ui.label(RichText::new(format!("{} · {} Hz · {} ch", a.device_name(), a.sample_rate(), a.channels())).size(theme::type_scale::SMALL).color(c));
                if a.leaked() > 0 {
                    ui.colored_label(theme::standby(), RichText::new(format!("RT queue overflow ×{}", a.leaked())).size(theme::type_scale::SMALL));
                }
                if a.underruns() > 0 {
                    ui.colored_label(theme::standby(), RichText::new(format!("Disk underrun ×{}", a.underruns())).size(theme::type_scale::SMALL))
                        .on_hover_text("A long file streamed from disk ran dry and played silence. Check the drive (or move the show to a faster one).");
                }
            }
            None => {
                ui.colored_label(theme::stop_text(), RichText::new("Audio offline").size(theme::type_scale::SMALL));
            }
        }
        if !st.deck_shows_system {
            separator(ui);
            let h = &st.health;
            ui.label(RichText::new(format!("Show {:.0} Hz · out {:.0} fps", h.hz, st.output_fps)).size(theme::type_scale::SMALL).color(theme::text_dim()));
        }
        if st.av_sync.layers > 0 && !st.av_sync.tight() {
            separator(ui);
            ui.colored_label(theme::standby(), RichText::new(format!("A/V lock ±{:.1} ms", st.av_sync.worst_ms)).size(theme::type_scale::SMALL));
        }
        let w = st.watch;
        if w.notable() {
            separator(ui);
            let text = if w.show_stalled {
                "Show thread stalled".to_string()
            } else if w.audio_stalled {
                "Audio device stalled".to_string()
            } else if w.output_stalled {
                "Outputs stalled".to_string()
            } else if w.autosave_failures > 0 {
                "Autosave failing".to_string()
            } else {
                format!("Watchdog: {} stall(s), {} fault(s)", w.stalls, w.faults)
            };
            let color = if w.alarm() { theme::stop_text() } else { theme::standby() };
            ui.colored_label(color, RichText::new(text).size(theme::type_scale::SMALL)).on_hover_text(super::transport::watch_detail(&w));
        }
        if !livestage_video::capabilities::movie_support().movies_available() {
            separator(ui);
            ui.colored_label(theme::stop_text(), RichText::new("Movies off").size(theme::type_scale::SMALL))
                .on_hover_text(livestage_video::capabilities::movie_support().summary());
        }
        // Right: the log toggle and the latest line.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let warnings = rt.log.iter().filter(|l| l.level != LogLevel::Info).count();
            let label = match (st.log_open, warnings) {
                (true, _) => "Log ▾".to_string(),
                (false, 0) => "Log ▴".to_string(),
                (false, n) => format!("Log ▴ · {n}"),
            };
            let tone = if warnings > 0 && !st.log_open { theme::standby() } else { theme::text_dim() };
            if ui.add(egui::Button::new(RichText::new(label).size(theme::type_scale::SMALL).color(tone)).small()).on_hover_text("Show the operator log").clicked() {
                st.log_open = !st.log_open;
            }
            if let Some(last) = rt.log.back().filter(|_| !st.log_open) {
                ui.add(egui::Label::new(RichText::new(&last.text).size(theme::type_scale::SMALL).color(level_color(last.level))).truncate());
            }
        });
    });
}

/// The operator log drawer above the status bar.
pub fn log_drawer(ui: &mut Ui, rt: &UiRuntime, st: &mut UiState) {
    ui.horizontal(|ui| {
        ui.label(super::widgets::caption_text("Operator log"));
        ui.label(RichText::new(format!("{} line(s)", rt.log.len())).size(theme::type_scale::SMALL).color(theme::text_faint()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Close").clicked() {
                st.log_open = false;
            }
            super::widgets::inputs::segmented(ui, &mut st.log_filter, &[(None, "All"), (Some(LogLevel::Warn), "Warnings"), (Some(LogLevel::Error), "Errors")]);
        });
    });
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let keep = |l: LogLevel| match st.log_filter {
            None => true,
            Some(LogLevel::Warn) => l != LogLevel::Info,
            Some(f) => l == f,
        };
        for l in rt.log.iter().filter(|l| keep(l.level)) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(l.at.to_clock_string()).font(theme::clock_font(11.5)).color(theme::text_faint()));
                let tag = match l.level {
                    LogLevel::Info => "INFO",
                    LogLevel::Warn => "WARN",
                    LogLevel::Error => "ERROR",
                };
                ui.label(RichText::new(tag).font(theme::clock_font(10.5)).color(level_color(l.level)));
                ui.add(egui::Label::new(RichText::new(&l.text).color(if l.level == LogLevel::Info { theme::text() } else { level_color(l.level) })).wrap());
            });
        }
    });
}
