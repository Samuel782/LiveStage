//! Settings / Preferences window (⌘, on macOS, Ctrl+, elsewhere).
//!
//! Two kinds of settings, labelled as such: *this Mac* (preferences file)
//! and *this show* (saved in the show bundle so they travel with it).

use super::widgets::toggle;
use super::UiState;
use crate::prefs::Preferences;
use crate::view::UiRuntime;
use crate::theme::{self, Accent};
use egui::{ComboBox, DragValue, RichText, Ui};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    General,
    Audio,
    Control,
    Timecode,
    Outputs,
    Files,
}

impl Section {
    /// Parses a section name (case-insensitive prefix), for the snapshot harness.
    pub fn from_name(name: &str) -> Section {
        let n = name.to_lowercase();
        Self::ALL.into_iter().find(|s| s.title().to_lowercase().starts_with(&n)).unwrap_or_default()
    }

    const ALL: [Section; 6] = [Section::General, Section::Audio, Section::Control, Section::Timecode, Section::Outputs, Section::Files];
    fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Audio => "Audio",
            Section::Control => "Control",
            Section::Timecode => "Timecode",
            Section::Outputs => "Outputs & Video",
            Section::Files => "Files & Paths",
        }
    }
}

pub fn window(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
    if !st.settings_open {
        return;
    }
    let before = st.prefs.clone();
    let mut open = true;
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([760.0, 520.0])
        .min_size([620.0, 420.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                sidebar(ui, st);
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_min_width(440.0);
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match st.settings_section {
                        Section::General => general(ui, st),
                        Section::Audio => audio(ui, rt, st),
                        Section::Control => control(ui, rt, st),
                        Section::Timecode => timecode(ui, rt, st),
                        Section::Outputs => outputs(ui, st),
                        Section::Files => files(ui, rt, st),
                    });
                });
            });
        });
    if !open {
        st.settings_open = false;
    }
    if st.prefs != before {
        apply_prefs(ctx, &st.prefs);
        rt.office_path = st.prefs.office_path.clone();
        if let Err(e) = st.prefs.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
    }
}

/// Applies the live-adjustable preferences to the UI.
pub fn apply_prefs(ctx: &egui::Context, p: &Preferences) {
    theme::set_accent(p.accent);
    theme::apply(ctx);
    ctx.set_zoom_factor(p.ui_scale.clamp(0.7, 2.0));
}

fn sidebar(ui: &mut Ui, st: &mut UiState) {
    ui.vertical(|ui| {
        ui.set_width(150.0);
        for s in Section::ALL {
            let selected = st.settings_section == s;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(150.0, 28.0), egui::Sense::click());
            if selected {
                ui.painter().rect_filled(rect, 6.0, theme::accent_menu());
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 6.0, theme::PANEL_RAISED);
            }
            let color = if selected { egui::Color32::WHITE } else { theme::TEXT };
            ui.painter().text(rect.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, s.title(), egui::FontId::proportional(14.0), color);
            if resp.clicked() {
                st.settings_section = s;
            }
        }
    });
}

fn heading(ui: &mut Ui, title: &str, scope: &str) {
    ui.horizontal(|ui| {
        ui.heading(title);
        ui.label(super::widgets::caption_text(scope));
    });
    ui.add_space(4.0);
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(theme::TEXT_DIM));
}

fn path_field(ui: &mut Ui, id: &str, value: &mut Option<PathBuf>, placeholder: &str) {
    let mut text = value.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
    let r = ui.add(egui::TextEdit::singleline(&mut text).id_salt(id).hint_text(placeholder).desired_width(360.0));
    if r.changed() {
        let t = text.trim().trim_matches(|c| c == '"' || c == '\'').replace("\\ ", " ");
        *value = (!t.is_empty()).then(|| PathBuf::from(t));
    }
    if let Some(p) = value.as_ref().filter(|p| !p.exists()) {
        ui.colored_label(theme::standby(), format!("Not found: {}", p.display()));
    }
}

fn general(ui: &mut Ui, st: &mut UiState) {
    heading(ui, "General", "this Mac");
    egui::Grid::new("set-general").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Accent colour");
        ui.horizontal(|ui| {
            for a in Accent::ALL {
                let selected = st.prefs.accent == a;
                theme::set_accent(a);
                let c = theme::accent();
                theme::set_accent(st.prefs.accent);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
                ui.painter().circle_filled(rect.center(), 10.0, c);
                if selected {
                    ui.painter().circle_stroke(rect.center(), 12.5, egui::Stroke::new(2.0_f32, theme::TEXT));
                }
                if resp.on_hover_text(a.label()).clicked() {
                    st.prefs.accent = a;
                }
            }
        });
        ui.end_row();
        ui.label("Interface size");
        ui.add(egui::Slider::new(&mut st.prefs.ui_scale, 0.8..=1.6).step_by(0.05).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
        ui.end_row();
        ui.label("Quitting");
        toggle(ui, &mut st.prefs.confirm_quit_when_live, "Ask before quitting while outputs are live");
        ui.end_row();
        ui.label("Crash recovery");
        ui.horizontal(|ui| {
            ui.add(DragValue::new(&mut st.prefs.autosave_secs).range(0..=600).suffix(" s"));
            hint(ui, "autosave interval (0 = off)");
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    hint(ui, "State colours (standby amber, active / GO green, stop red) are fixed so they always mean the same thing.");
}

fn audio(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    heading(ui, "Audio", "this show");
    if st.audio_devices.is_empty() {
        st.audio_devices = livestage_audio::list_output_devices();
    }
    let mut changed = false;
    let r = &mut rt.bundle.project.audio;
    egui::Grid::new("set-audio").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Output device");
        ui.horizontal(|ui| {
            let current = r.device_name.clone().unwrap_or_else(|| "System default".into());
            ComboBox::from_id_salt("set-adev").width(300.0).selected_text(current).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut r.device_name, None, "System default").changed();
                for d in &st.audio_devices {
                    let label = format!("{} ({} ch, {} Hz)", d.name, d.max_channels, d.default_sample_rate);
                    changed |= ui.selectable_value(&mut r.device_name, Some(d.name.clone()), label).changed();
                }
            });
            if ui.small_button("Rescan").clicked() {
                st.audio_devices = livestage_audio::list_output_devices();
            }
        });
        ui.end_row();
        ui.label("Sample rate");
        let rate_label = |r: Option<u32>| r.map_or("Device default".to_string(), |r| format!("{r} Hz"));
        ComboBox::from_id_salt("set-rate").selected_text(rate_label(r.sample_rate)).show_ui(ui, |ui| {
            for rate in [None, Some(44_100), Some(48_000), Some(88_200), Some(96_000)] {
                changed |= ui.selectable_value(&mut r.sample_rate, rate, rate_label(rate)).changed();
            }
        });
        ui.end_row();
        ui.label("Buffer size");
        ui.horizontal(|ui| {
            let buf_label = |b: Option<u32>| b.map_or("Device default".to_string(), |b| format!("{b} frames"));
            ComboBox::from_id_salt("set-buf").selected_text(buf_label(r.buffer_frames)).show_ui(ui, |ui| {
                for b in [None, Some(32), Some(64), Some(128), Some(256), Some(512), Some(1024), Some(2048)] {
                    changed |= ui.selectable_value(&mut r.buffer_frames, b, buf_label(b)).changed();
                }
            });
            if let Some(b) = r.buffer_frames {
                let rate = rt.audio.as_ref().map_or(r.sample_rate.unwrap_or(48_000), |a| a.sample_rate()).max(1);
                hint(ui, &format!("≈ {:.1} ms per buffer", b as f64 * 1000.0 / rate as f64));
            }
        });
        ui.end_row();
        ui.label("Show outputs");
        changed |= ui.add(DragValue::new(&mut r.output_channels).range(1..=64).suffix(" channels")).changed();
        ui.end_row();
    });
    hint(ui, "Smaller buffers lower latency but need more headroom; 128–256 frames suit most interfaces.");
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(RichText::new("Apply & restart audio engine").strong()).clicked() {
            rt.start_audio();
            changed = true;
        }
        match (&rt.audio, &rt.audio_error) {
            (Some(a), _) => {
                ui.colored_label(theme::active(), format!("Running: {} · {} Hz · {} ch", a.device_name(), a.sample_rate(), a.channels()));
            }
            (None, Some(e)) => {
                ui.colored_label(theme::stop(), e);
            }
            _ => {}
        }
    });
    hint(ui, "Restarting the engine stops any audio that is playing.");
    if changed {
        rt.mark_changed();
    }
}

fn control(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    heading(ui, "Control", "this show");
    if st.midi_inputs.is_empty() {
        st.midi_inputs = livestage_control::midi::list_inputs();
    }
    let mut changed = false;
    let c = &mut rt.bundle.project.control;
    egui::Grid::new("set-control").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("OSC input");
        ui.horizontal(|ui| {
            changed |= toggle(ui, &mut c.osc_enabled, "enabled").changed();
            changed |= ui.add_enabled(c.osc_enabled, DragValue::new(&mut c.osc_listen_port).range(1024..=65_535).prefix("UDP ")).changed();
        });
        ui.end_row();
        ui.label("WebSocket remote");
        ui.horizontal(|ui| {
            changed |= toggle(ui, &mut c.websocket_enabled, "enabled").changed();
            changed |= ui.add_enabled(c.websocket_enabled, DragValue::new(&mut c.websocket_port).range(1024..=65_535).prefix("TCP ")).changed();
        });
        ui.end_row();
        ui.label("MIDI input");
        ui.horizontal(|ui| {
            let current = c.midi_input.clone().unwrap_or_else(|| "First available".into());
            ComboBox::from_id_salt("set-midi").width(260.0).selected_text(current).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut c.midi_input, None, "First available").changed();
                for p in &st.midi_inputs {
                    changed |= ui.selectable_value(&mut c.midi_input, Some(p.clone()), p).changed();
                }
            });
            if ui.small_button("Rescan").clicked() {
                st.midi_inputs = livestage_control::midi::list_inputs();
            }
        });
        ui.end_row();
        ui.label("MIDI Show Control");
        ui.horizontal(|ui| {
            let mut all = c.msc_device_id == 0x7F;
            if toggle(ui, &mut all, "all device ids").changed() {
                c.msc_device_id = if all { 0x7F } else { 0 };
                changed = true;
            }
            if !all {
                changed |= ui.add(DragValue::new(&mut c.msc_device_id).range(0..=0x6F).prefix("device ")).changed();
            }
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(RichText::new("Apply & restart control services").strong()).clicked() {
            rt.restart_control_requested = true;
        }
        let osc = rt.osc_port().map_or("OSC off".to_string(), |p| format!("OSC :{p}"));
        let ws = rt.ws_port().map_or("WS off".to_string(), |p| format!("WebSocket :{p}"));
        let midi = rt.midi_in.as_ref().map_or("MIDI off".to_string(), |m| format!("MIDI: {}", m.port_name));
        ui.label(RichText::new(format!("{osc} · {ws} · {midi}")).color(theme::TEXT_DIM));
    });
    hint(ui, "OSC: /livestage/go, /livestage/cue/<n>/seek <secs>, /livestage/slide/next … (see README).");
    if changed {
        rt.mark_changed();
    }
}

fn timecode(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    use livestage_control::{ControlSource, ShowCommand};
    use livestage_core::timecode::{ClockBase, FrameRate, ListTimecode, LockState, OnTimecodeStop, TimecodeSource};
    heading(ui, "Timecode", "this show");
    if st.midi_inputs.is_empty() {
        st.midi_inputs = livestage_control::midi::list_inputs();
    }

    // Live readout
    let t = rt.timecode.transport();
    let (color, state) = match t.state {
        LockState::Locked => (theme::active(), if t.running { "Rolling" } else { "Locked" }),
        LockState::Freewheel => (theme::standby(), "Freewheel"),
        LockState::Locking => (theme::standby(), "Locking…"),
        LockState::Stopped => (theme::TEXT_DIM, "Stopped"),
    };
    egui::Frame::new().fill(theme::PANEL_RAISED).corner_radius(8).inner_margin(10).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(t.timecode().to_string()).font(theme::clock_font(34.0)).color(if t.running { theme::active() } else { theme::TEXT }));
            ui.vertical(|ui| {
                ui.colored_label(color, RichText::new(state).strong());
                ui.label(RichText::new(format!("{} · {}", t.rate, rt.timecode_input)).size(12.0).color(theme::TEXT_DIM));
                if rt.timecode.config().source != TimecodeSource::Internal && t.state != LockState::Stopped {
                    ui.label(
                        RichText::new(format!("drift {:+.0} ppm · jitter {:+.1} ms", t.drift_ppm(), rt.timecode.lock_error_us() / 1000.0))
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                }
            });
        });
        if rt.timecode.config().source == TimecodeSource::Internal {
            ui.horizontal(|ui| {
                if ui.button(if t.running { "Stop" } else { "Start" }).clicked() {
                    rt.command(ControlSource::Ui, if t.running { ShowCommand::TimecodeStop } else { ShowCommand::TimecodeStart });
                }
                if ui.button("Reset").on_hover_text("Stop and park at the start timecode").clicked() {
                    rt.timecode.reset();
                }
                ui.separator();
                ui.label("Locate");
                // Shows the live position; typing a label and pressing Enter locates.
                let mut target = t.timecode();
                if super::widgets::timecode_field(ui, "set-tc-locate", &mut target, t.rate) {
                    rt.command(ControlSource::Ui, ShowCommand::TimecodeLocate { timecode: target });
                }
            });
        }
    });
    ui.add_space(8.0);

    let mut changed = false;
    let mut cfg = rt.bundle.project.timecode.clone();
    egui::Grid::new("set-tc").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Source");
        ComboBox::from_id_salt("set-tc-src").width(220.0).selected_text(cfg.source.label()).show_ui(ui, |ui| {
            for src in TimecodeSource::ALL {
                changed |= ui.selectable_value(&mut cfg.source, src, src.label()).changed();
            }
        });
        ui.end_row();
        match cfg.source {
            TimecodeSource::Internal => {
                ui.label("Frame rate");
                ComboBox::from_id_salt("set-tc-rate").selected_text(cfg.rate.label()).show_ui(ui, |ui| {
                    for r in FrameRate::ALL {
                        changed |= ui.selectable_value(&mut cfg.rate, r, r.label()).changed();
                    }
                });
                ui.end_row();
                ui.label("Clock");
                ComboBox::from_id_salt("set-tc-base").selected_text(cfg.internal_base.label()).show_ui(ui, |ui| {
                    for b in [ClockBase::Audio, ClockBase::Wall] {
                        changed |= ui.selectable_value(&mut cfg.internal_base, b, b.label()).changed();
                    }
                });
                ui.end_row();
                ui.label("Start at");
                changed |= super::widgets::timecode_field(ui, "set-tc-start", &mut cfg.internal_start, cfg.rate);
                ui.end_row();
            }
            TimecodeSource::Mtc => {
                ui.label("MIDI input");
                ui.horizontal(|ui| {
                    let current = cfg.mtc_input.clone().unwrap_or_else(|| "First available".into());
                    ComboBox::from_id_salt("set-tc-midi").width(260.0).selected_text(current).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut cfg.mtc_input, None, "First available").changed();
                        for p in &st.midi_inputs {
                            changed |= ui.selectable_value(&mut cfg.mtc_input, Some(p.clone()), p).changed();
                        }
                    });
                    if ui.small_button("Rescan").clicked() {
                        st.midi_inputs = livestage_control::midi::list_inputs();
                    }
                });
                ui.end_row();
            }
            TimecodeSource::ArtNet => {
                ui.label("Art-Net port");
                changed |= ui.add(DragValue::new(&mut cfg.artnet_port).range(1024..=65_535).prefix("UDP ")).changed();
                ui.end_row();
            }
        }
        if cfg.source != TimecodeSource::Internal {
            ui.label("Freewheel");
            ui.horizontal(|ui| {
                let mut ms = cfg.lock.freewheel.as_micros() / 1000;
                if ui.add(DragValue::new(&mut ms).range(0..=5_000).suffix(" ms")).changed() {
                    cfg.lock.freewheel = livestage_core::ShowTime::from_millis(ms);
                    changed = true;
                }
                hint(ui, "keep rolling through dropouts this long");
            });
            ui.end_row();
            ui.label("Jump threshold");
            ui.horizontal(|ui| {
                changed |= ui.add(DragValue::new(&mut cfg.lock.jump_frames).range(1..=100).suffix(" frames")).changed();
                hint(ui, "larger errors re-locate instead of drifting back");
            });
            ui.end_row();
        }
    });
    if changed {
        rt.bundle.project.timecode = cfg;
        rt.apply_timecode_config();
        rt.mark_changed();
    }

    ui.add_space(10.0);
    ui.label(RichText::new("Cue lists in timecode mode").strong());
    hint(ui, "Cues with a timecode trigger (Inspector → Timecode) fire on their own; chasing, pausing and re-sync follow the stream.");
    let mut list_changed = false;
    for list in &mut rt.bundle.project.cue_lists {
        ui.horizontal(|ui| {
            let mut on = list.timecode.is_some();
            if toggle(ui, &mut on, &list.name).changed() {
                list.timecode = on.then(ListTimecode::new);
                list_changed = true;
            }
            if let Some(tc) = &mut list.timecode {
                ComboBox::from_id_salt(("set-tc-list", list.id)).selected_text(format!("on stop: {}", tc.on_stop.label())).show_ui(ui, |ui| {
                    for o in OnTimecodeStop::ALL {
                        list_changed |= ui.selectable_value(&mut tc.on_stop, o, o.label()).changed();
                    }
                });
                let triggers = list.cues.iter().filter(|c| livestage_core::cue::Cue::header(*c).timecode.is_some()).count();
                hint(ui, &format!("{triggers} triggered cue(s)"));
            }
        });
    }
    if list_changed {
        rt.mark_changed();
    }
    ui.add_space(6.0);
    hint(ui, "Timeline cues become slaves in the Inspector (Timecode slave). OSC: /livestage/timecode/start | stop | locate \"HH:MM:SS:FF\".");
}

fn outputs(ui: &mut Ui, st: &mut UiState) {
    heading(ui, "Outputs & Video", "this Mac");
    let p = &mut st.prefs;
    egui::Grid::new("set-outputs").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Program outputs");
        ui.vertical(|ui| {
            toggle(ui, &mut p.outputs_on_launch, "Open outputs when a show is loaded");
            toggle(ui, &mut p.outputs_always_on_top, "Keep kiosk outputs above other windows");
            toggle(ui, &mut p.hide_cursor_on_outputs, "Hide the mouse pointer on kiosk outputs");
        });
        ui.end_row();
        ui.label("Movie playback");
        ui.vertical(|ui| {
            use livestage_video::capabilities::{movie_support, MovieSupport};
            let media = movie_support();
            match media.warning() {
                Some(w) => {
                    ui.colored_label(theme::standby(), w);
                }
                None => {
                    ui.label(media.summary());
                }
            }
            if let MovieSupport::Ready { missing_optional, .. } = media {
                if !missing_optional.is_empty() {
                    hint(ui, &format!("Not in this FFmpeg build: {}", missing_optional.join(", ")));
                }
            }
            if !media.movies_available() {
                hint(ui, "Release builds include FFmpeg (scripts/build-release.sh); see README → Building.");
            }
        });
        ui.end_row();
        ui.label("Video decoding");
        ui.vertical(|ui| {
            toggle(ui, &mut p.hardware_decode, "Hardware decoding (VideoToolbox / D3D11VA / VAAPI)");
            hint(ui, "Takes effect after restarting LiveStage.");
        });
        ui.end_row();
        ui.label("NDI runtime folder");
        ui.vertical(|ui| {
            path_field(ui, "set-ndi", &mut p.ndi_runtime_dir, "SDK default locations");
            let found = livestage_video::ndi::detect_runtime();
            match found {
                Some(f) => hint(ui, &format!("In use: {}", f.display())),
                None => {
                    ui.colored_label(theme::standby(), "NDI runtime not found — install NDI Tools / the NDI runtime.");
                }
            }
            hint(ui, "Takes effect after restarting LiveStage.");
        });
        ui.end_row();
    });
    hint(ui, "Window changes apply the next time outputs are opened.");
}

fn files(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    heading(ui, "Files & Paths", "this Mac");
    let root = rt.bundle.root().to_path_buf();
    let p = &mut st.prefs;
    egui::Grid::new("set-files").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Show at startup");
        ui.vertical(|ui| {
            path_field(ui, "set-startup", &mut p.startup_show, "Demo show");
            if ui.small_button("Use the current show").clicked() {
                p.startup_show = Some(root.clone());
            }
        });
        ui.end_row();
        ui.label("Media folder");
        ui.vertical(|ui| {
            path_field(ui, "set-media", &mut p.media_folder, "pre-fills the import dialog");
        });
        ui.end_row();
        ui.label("LibreOffice");
        ui.vertical(|ui| {
            path_field(ui, "set-office", &mut p.office_path, "auto-detect soffice");
            match p.office_path.clone().filter(|x| x.is_file()).or_else(livestage_video::slides::find_office) {
                Some(o) => hint(ui, &format!("PowerPoint / Keynote import via {}", o.display())),
                None => hint(ui, "Not installed: .pptx uses the built-in renderer; .ppt / .key / .odp need LibreOffice."),
            }
        });
        ui.end_row();
        ui.label("Current show");
        ui.label(RichText::new(root.display().to_string()).color(theme::TEXT_DIM));
        ui.end_row();
        ui.label("Preferences file");
        ui.label(RichText::new(Preferences::path().map_or("—".into(), |p| p.display().to_string())).color(theme::TEXT_DIM));
        ui.end_row();
    });
}
