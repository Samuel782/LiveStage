//! Cue inspector: edits the selected cue (including timeline children).

use super::widgets::{color_edit, opt_time_edit, time_edit};
use super::UiState;
use crate::runtime::ShowRuntime;
use crate::theme;
use egui::{ComboBox, DragValue, RichText, TextEdit, Ui};
use livestage_core::action::*;
use livestage_core::cue::*;
use livestage_core::id::{OutputId, StemId};
use livestage_core::project::AssetRef;
use livestage_core::ShowTime;

pub fn show(ui: &mut Ui, rt: &mut ShowRuntime, st: &mut UiState) {
    let Some(id) = st.selected else {
        ui.label(RichText::new("Select a cue to edit it.").color(theme::TEXT_DIM));
        return;
    };
    let outputs: Vec<(OutputId, String)> = rt.bundle.project.displays.iter().map(|d| (d.id, d.name.clone())).collect();
    let stems: Vec<(StemId, String)> = rt.bundle.project.audio.stems.iter().map(|s| (s.id, s.name.clone())).collect();
    let assets: Vec<AssetRef> = rt.bundle.project.assets.iter().map(|a| a.path.clone()).collect();
    let next_child = rt.next_cue_number();
    // Only timelines need project data while the cue is mutably borrowed.
    let is_timeline = rt.bundle.project.active_list().and_then(|l| l.find(id)).is_some_and(|c| matches!(c, AnyCue::Timeline(_)));
    let project_snapshot = is_timeline.then(|| rt.bundle.project.clone());
    let Some(cue) = rt.find_cue_mut(id) else {
        st.selected = None;
        return;
    };
    let mut changed = false;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.heading(format!("{} cue", cue.kind().label()));
        let state = cue.state();
        if state.is_active() {
            ui.colored_label(theme::standby(), "Running — edits apply on next GO");
        }
        egui::Grid::new("hdr").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            let h = cue.header_mut();
            ui.label("Number");
            changed |= ui.add(TextEdit::singleline(&mut h.number).desired_width(80.0)).changed();
            ui.end_row();
            ui.label("Name");
            changed |= ui.add(TextEdit::singleline(&mut h.name).desired_width(f32::INFINITY)).changed();
            ui.end_row();
            ui.label("Notes");
            changed |= ui.add(TextEdit::multiline(&mut h.notes).desired_rows(2).desired_width(f32::INFINITY)).changed();
            ui.end_row();
            ui.label("Pre-wait");
            changed |= time_edit(ui, &mut h.pre_wait);
            ui.end_row();
            ui.label("Follow");
            ComboBox::from_id_salt("follow").selected_text(format!("{:?}", h.follow)).show_ui(ui, |ui| {
                for f in [FollowMode::None, FollowMode::AutoContinue, FollowMode::AutoFollow] {
                    changed |= ui.selectable_value(&mut h.follow, f, format!("{f:?}")).changed();
                }
            });
            ui.end_row();
            ui.label("Post-wait");
            changed |= time_edit(ui, &mut h.post_wait);
            ui.end_row();
            ui.label("Armed");
            changed |= ui.checkbox(&mut h.armed, "").changed();
            ui.end_row();
        });
        ui.separator();
        changed |= match cue {
            AnyCue::Audio(a) => audio(ui, a, &stems, &assets),
            AnyCue::Video(v) => video(ui, v, &outputs, &assets),
            AnyCue::Timer(t) => timer(ui, t, &outputs),
            AnyCue::Dmx(d) => dmx(ui, d),
            AnyCue::Control(c) => control(ui, c),
            AnyCue::Timeline(t) => match &project_snapshot {
                Some(p) => timeline(ui, t, p, &next_child, st),
                None => false,
            },
        };
        let issues = cue.validate();
        for i in issues {
            ui.colored_label(theme::stop(), format!("Problem: {i}"));
        }
    });
    if changed {
        rt.mark_changed();
    }
}

fn output_combo(ui: &mut Ui, id: &str, value: &mut OutputId, outputs: &[(OutputId, String)]) -> bool {
    let mut changed = false;
    let current = outputs.iter().find(|o| o.0 == *value).map_or("— none —".to_string(), |o| o.1.clone());
    ComboBox::from_id_salt(id).selected_text(current).show_ui(ui, |ui| {
        for (oid, name) in outputs {
            changed |= ui.selectable_value(value, *oid, name).changed();
        }
    });
    changed
}

fn asset_combo(ui: &mut Ui, id: impl std::hash::Hash, value: &mut AssetRef, assets: &[AssetRef]) -> bool {
    let mut changed = false;
    ComboBox::from_id_salt(id).width(220.0).selected_text(value.file_name().to_string()).show_ui(ui, |ui| {
        for a in assets {
            changed |= ui.selectable_value(value, a.clone(), a.as_str()).changed();
        }
    });
    changed
}

fn audio(ui: &mut Ui, a: &mut AudioCue, stems: &[(StemId, String)], assets: &[AssetRef]) -> bool {
    let mut c = false;
    egui::Grid::new("audio").num_columns(2).show(ui, |ui| {
        ui.label("Gain");
        c |= ui.add(DragValue::new(&mut a.gain_db).range(-96.0..=12.0).speed(0.1).suffix(" dB")).changed();
        ui.end_row();
        ui.label("Fade in");
        c |= time_edit(ui, &mut a.fade_in);
        ui.end_row();
        ui.label("Fade out");
        c |= time_edit(ui, &mut a.fade_out);
        ui.end_row();
        ui.label("Start at");
        c |= time_edit(ui, &mut a.start_at);
        ui.end_row();
        ui.label("End at");
        c |= opt_time_edit(ui, &mut a.end_at, "file end");
        ui.end_row();
        ui.label("Loop");
        c |= ui.checkbox(&mut a.looped, "").changed();
        ui.end_row();
    });
    ui.label(RichText::new("Tracks / stems (start sample-synchronously)").strong());
    let mut remove = None;
    for (i, t) in a.tracks.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            c |= asset_combo(ui, ("trk", i), &mut t.asset, assets);
            let name = stems.iter().find(|s| s.0 == t.stem).map_or("?".into(), |s| s.1.clone());
            ComboBox::from_id_salt(("stem", i)).selected_text(name).show_ui(ui, |ui| {
                for (sid, n) in stems {
                    c |= ui.selectable_value(&mut t.stem, *sid, n).changed();
                }
            });
            c |= ui.add(DragValue::new(&mut t.gain_db).range(-96.0..=12.0).suffix(" dB")).changed();
            c |= ui.checkbox(&mut t.mute, "mute").changed();
            if ui.small_button("✖").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        a.tracks.remove(i);
        c = true;
    }
    if ui.button("+ Track").clicked() {
        if let (Some(asset), Some(stem)) = (assets.first(), stems.first()) {
            a.tracks.push(AudioTrack { asset: asset.clone(), stem: stem.0, gain_db: 0.0, mute: false });
            c = true;
        }
    }
    ui.separator();
    let mut has_click = a.click.is_some();
    if ui.checkbox(&mut has_click, RichText::new("Click track").strong()).changed() {
        a.click = has_click.then(|| ClickTrack {
            bpm: 120.0,
            beats_per_bar: 4,
            stem: stems.iter().find(|s| s.1 == "Click").or(stems.first()).map(|s| s.0).unwrap_or_default(),
            gain_db: -6.0,
            count_in_bars: 1,
        });
        c = true;
    }
    if let Some(k) = &mut a.click {
        ui.horizontal(|ui| {
            c |= ui.add(DragValue::new(&mut k.bpm).range(20.0..=400.0).speed(0.5).suffix(" bpm")).changed();
            c |= ui.add(DragValue::new(&mut k.beats_per_bar).range(1..=16).suffix("/bar")).changed();
            c |= ui.add(DragValue::new(&mut k.count_in_bars).range(0..=8).suffix(" bar count-in")).changed();
            c |= ui.add(DragValue::new(&mut k.gain_db).range(-60.0..=6.0).suffix(" dB")).changed();
        });
    }
    c
}

fn video(ui: &mut Ui, v: &mut VideoCue, outputs: &[(OutputId, String)], assets: &[AssetRef]) -> bool {
    let mut c = false;
    egui::Grid::new("video").num_columns(2).show(ui, |ui| {
        ui.label("Output");
        c |= output_combo(ui, "vout", &mut v.output, outputs);
        ui.end_row();
        ui.label("Layer");
        c |= ui.add(DragValue::new(&mut v.layer)).changed();
        ui.end_row();
        ui.label("Opacity");
        c |= ui.add(egui::Slider::new(&mut v.opacity, 0.0..=1.0)).changed();
        ui.end_row();
        ui.label("Fade in / out");
        ui.horizontal(|ui| {
            c |= time_edit(ui, &mut v.fade_in);
            c |= time_edit(ui, &mut v.fade_out);
        });
        ui.end_row();
        ui.label("Hold");
        c |= opt_time_edit(ui, &mut v.hold, "until stopped");
        ui.end_row();
        ui.label("At end");
        ComboBox::from_id_salt("endb").selected_text(format!("{:?}", v.end_behavior)).show_ui(ui, |ui| {
            c |= ui.selectable_value(&mut v.end_behavior, EndBehavior::Hide, "Hide").changed();
            c |= ui.selectable_value(&mut v.end_behavior, EndBehavior::HoldLastFrame, "Hold last frame").changed();
        });
        ui.end_row();
    });
    ui.separator();
    let kind = match &v.content {
        VisualContent::Video { .. } => "Video",
        VisualContent::Image { .. } => "Image",
        VisualContent::Text(_) => "Text / Teleprompter",
        VisualContent::Timer(_) => "Timer",
        VisualContent::Ndi { .. } => "NDI",
        VisualContent::Color { .. } => "Colour",
        VisualContent::TestPattern => "Test pattern",
    };
    ComboBox::from_id_salt("content").selected_text(kind).show_ui(ui, |ui| {
        let first = assets.first().cloned().unwrap_or_else(|| AssetRef::new("assets/video/missing.mov"));
        let choices = [
            ("Video", VisualContent::Video { asset: first.clone(), looped: false }),
            ("Image", VisualContent::Image { asset: first }),
            (
                "Text / Teleprompter",
                VisualContent::Text(TextContent { text: "Text".into(), font_size: 72.0, color: [1.0; 4], scroll_speed: 0.0, mirror_horizontal: false }),
            ),
            ("NDI", VisualContent::Ndi { source_name: "MACHINE (Source)".into() }),
            ("Colour", VisualContent::Color { rgba: [1.0, 1.0, 1.0, 1.0] }),
            ("Test pattern", VisualContent::TestPattern),
        ];
        for (label, content) in choices {
            if ui.selectable_label(kind == label, label).clicked() && kind != label {
                v.content = content;
                c = true;
            }
        }
    });
    match &mut v.content {
        VisualContent::Video { asset, looped } => {
            c |= asset_combo(ui, "vasset", asset, assets);
            c |= ui.checkbox(looped, "Loop").changed();
        }
        VisualContent::Image { asset } => c |= asset_combo(ui, "iasset", asset, assets),
        VisualContent::Text(t) => {
            c |= ui.add(TextEdit::multiline(&mut t.text).desired_rows(4).desired_width(f32::INFINITY)).changed();
            ui.horizontal(|ui| {
                c |= ui.add(DragValue::new(&mut t.font_size).range(8.0..=600.0).suffix(" pt")).changed();
                c |= ui.add(DragValue::new(&mut t.scroll_speed).range(0.0..=500.0).suffix(" pt/s scroll")).changed();
                c |= ui.checkbox(&mut t.mirror_horizontal, "Mirror (prompter glass)").changed();
                c |= color_edit(ui, &mut t.color);
            });
        }
        VisualContent::Ndi { source_name } => {
            ui.horizontal(|ui| {
                c |= ui.text_edit_singleline(source_name).changed();
                let sources = livestage_video::ndi::discovered_sources();
                ComboBox::from_id_salt("ndi-src").selected_text(format!("{} on network", sources.len())).show_ui(ui, |ui| {
                    for src in &sources {
                        if ui.selectable_label(*source_name == src.name, &src.name).clicked() {
                            *source_name = src.name.clone();
                            c = true;
                        }
                    }
                    if sources.is_empty() {
                        ui.weak(if livestage_video::ndi::runtime().is_some() { "searching…" } else { "NDI runtime not installed" });
                    }
                });
            });
        }
        VisualContent::Color { rgba } => c |= color_edit(ui, rgba),
        VisualContent::Timer(_) | VisualContent::TestPattern => {}
    }
    c
}

fn timer(ui: &mut Ui, t: &mut TimerCue, outputs: &[(OutputId, String)]) -> bool {
    let mut c = false;
    egui::Grid::new("timer").num_columns(2).show(ui, |ui| {
        ui.label("Output");
        c |= output_combo(ui, "tout", &mut t.output, outputs);
        ui.end_row();
        ui.label("Label");
        c |= ui.text_edit_singleline(&mut t.timer.label).changed();
        ui.end_row();
        ui.label("Mode");
        let label = match t.timer.mode {
            TimerMode::CountUp => "Count up",
            TimerMode::CountDown { .. } => "Countdown",
            TimerMode::TimeOfDay => "Time of day",
        };
        ComboBox::from_id_salt("tmode").selected_text(label).show_ui(ui, |ui| {
            if ui.selectable_label(label == "Count up", "Count up").clicked() {
                t.timer.mode = TimerMode::CountUp;
                c = true;
            }
            if ui.selectable_label(label == "Countdown", "Countdown").clicked() {
                t.timer.mode = TimerMode::CountDown { from: ShowTime::from_secs(300) };
                c = true;
            }
            if ui.selectable_label(label == "Time of day", "Time of day").clicked() {
                t.timer.mode = TimerMode::TimeOfDay;
                c = true;
            }
        });
        ui.end_row();
        if let TimerMode::CountDown { from } = &mut t.timer.mode {
            ui.label("From");
            c |= time_edit(ui, from);
            ui.end_row();
            ui.label("Warn below");
            c |= time_edit(ui, &mut t.timer.warn_below);
            ui.end_row();
        }
        ui.label("Font size");
        c |= ui.add(DragValue::new(&mut t.timer.font_size).range(12.0..=800.0)).changed();
        ui.end_row();
        ui.label("Colours");
        ui.horizontal(|ui| {
            c |= color_edit(ui, &mut t.timer.color);
            c |= color_edit(ui, &mut t.timer.warn_color);
        });
        ui.end_row();
        ui.label("Hold at zero");
        c |= ui.checkbox(&mut t.hold_at_zero, "").changed();
        ui.end_row();
    });
    c
}

fn dmx(ui: &mut Ui, d: &mut DmxCue) -> bool {
    let mut c = false;
    ui.horizontal(|ui| {
        ui.label("Fade");
        c |= time_edit(ui, &mut d.fade);
        c |= ui.checkbox(&mut d.release_on_stop, "Release on stop").changed();
    });
    ui.label(RichText::new("Channels").strong());
    let mut remove = None;
    egui::Grid::new("dmxch").num_columns(4).striped(true).show(ui, |ui| {
        ui.weak("Universe");
        ui.weak("Channel");
        ui.weak("Level");
        ui.end_row();
        for (i, ch) in d.channels.iter_mut().enumerate() {
            let mut u1 = ch.universe + 1;
            if ui.add(DragValue::new(&mut u1).range(1..=32768)).changed() {
                ch.universe = u1 - 1;
                c = true;
            }
            c |= ui.add(DragValue::new(&mut ch.channel).range(1..=512)).changed();
            let mut pct = ch.value as f32 / 255.0 * 100.0;
            if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%")).changed() {
                ch.value = (pct / 100.0 * 255.0).round() as u8;
                c = true;
            }
            if ui.small_button("✖").clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = remove {
        d.channels.remove(i);
        c = true;
    }
    if ui.button("+ Channel").clicked() {
        let next = d.channels.last().map_or(1, |c| (c.channel % 512) + 1);
        d.channels.push(DmxChannelValue { universe: 0, channel: next, value: 255 });
        c = true;
    }
    if !d.attributes.is_empty() {
        ui.label(RichText::new(format!("{} fixture attribute values", d.attributes.len())).color(theme::TEXT_DIM));
    }
    c
}

fn control(ui: &mut Ui, cc: &mut ControlCue) -> bool {
    let mut c = false;
    match &mut cc.action {
        ControlAction::SendOsc { host, port, address, .. } => {
            ui.label("Send OSC");
            ui.horizontal(|ui| {
                c |= ui.add(TextEdit::singleline(host).desired_width(120.0)).changed();
                c |= ui.add(DragValue::new(port)).changed();
                c |= ui.add(TextEdit::singleline(address).desired_width(180.0)).changed();
            });
        }
        ControlAction::SendMidi { bytes, .. } => {
            ui.label(format!("Send MIDI: {bytes:02X?}"));
        }
        ControlAction::Command(cmd) => {
            ui.label(format!("Show command: {cmd:?}"));
        }
        ControlAction::Marker { text } => {
            ui.label("Marker");
            c |= ui.text_edit_singleline(text).changed();
        }
    }
    ui.horizontal(|ui| {
        if ui.small_button("OSC").clicked() {
            cc.action = ControlAction::SendOsc { host: "127.0.0.1".into(), port: 53000, address: "/go".into(), args: vec![] };
            c = true;
        }
        if ui.small_button("Marker").clicked() {
            cc.action = ControlAction::Marker { text: "Marker".into() };
            c = true;
        }
        if ui.small_button("Stop all").clicked() {
            cc.action = ControlAction::Command(livestage_core::ShowCommand::StopAll { fade: Some(ShowTime::from_secs(2)) });
            c = true;
        }
    });
    c
}

fn timeline(ui: &mut Ui, t: &mut livestage_core::timeline::TimelineCue, p: &livestage_core::Project, next: &str, st: &mut UiState) -> bool {
    let mut c = false;
    ui.horizontal(|ui| {
        ui.label("Length");
        c |= opt_time_edit(ui, &mut t.length, "from master");
    });
    match &t.master {
        Some(m) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Master clock:").strong());
                if ui.link(format!("{} {}", m.header().number, m.header().name)).clicked() {
                    st.selected = Some(m.header().id);
                }
            });
        }
        None => {
            ui.label(RichText::new("No master — runs on the show clock").color(theme::TEXT_DIM));
            if ui.button("Add audio master").clicked() {
                t.master = Some(Box::new(crate::demo::new_cue(p, CueKind::Audio, &format!("{next}.0"))));
                c = true;
            }
        }
    }
    ui.label(RichText::new("Stacked items").strong());
    let mut remove = None;
    let mut resort = false;
    egui::Grid::new("tlitems").num_columns(3).striped(true).show(ui, |ui| {
        for (i, item) in t.items.iter_mut().enumerate() {
            if time_edit(ui, &mut item.offset) {
                c = true;
                resort = true;
            }
            if ui.link(format!("{} {} ({})", item.cue.header().number, item.cue.header().name, item.cue.kind().label())).clicked() {
                st.selected = Some(item.cue.header().id);
            }
            if ui.small_button("✖").clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if resort {
        t.items.sort_by_key(|i| i.offset);
    }
    if let Some(i) = remove {
        t.items.remove(i);
        c = true;
    }
    ui.horizontal(|ui| {
        let at = t.items.last().map_or(ShowTime::ZERO, |i| i.offset + ShowTime::from_secs(1));
        let n = t.items.len() + 1;
        for kind in [CueKind::Dmx, CueKind::Video, CueKind::Timer, CueKind::Control] {
            if ui.small_button(format!("+ {}", kind.label())).clicked() {
                let number = format!("{}.{n}", t.header.number);
                t.add(at, crate::demo::new_cue(p, kind, &number));
                c = true;
            }
        }
    });
    c
}
