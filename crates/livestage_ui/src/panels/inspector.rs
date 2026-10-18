//! Cue inspector: edits the selected cue (including timeline children).

use super::widgets::{color_edit, opt_time_edit, scrub_bar, time_edit};
use super::UiState;
use crate::runtime::{ShowRuntime, SlideNav};
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
    let playhead = rt.elapsed_of(id);
    let root = rt.bundle.root().to_path_buf();
    let mut seek_to: Option<ShowTime> = None;
    let mut slide_nav: Option<SlideNav> = None;
    // Only timelines need project data while the cue is mutably borrowed.
    let is_timeline = rt.bundle.project.active_list().and_then(|l| l.find(id)).is_some_and(|c| matches!(c, AnyCue::Timeline(_)));
    let project_snapshot = is_timeline.then(|| rt.bundle.project.clone());
    // DMX cues show palette / effect references by name.
    let lighting = rt.bundle.project.lighting.clone();
    let fixture_names: Vec<(livestage_core::id::FixtureId, String)> = rt.bundle.project.dmx.patch.iter().map(|f| (f.id, f.name.clone())).collect();
    // Timecode triggers apply to top-level cues of a list in timecode mode.
    let tc_rate = rt.bundle.project.timecode.rate;
    let (top_level, list_tc) = rt
        .bundle
        .project
        .active_list()
        .map_or((false, false), |l| (l.index_of(id).is_some(), l.timecode.is_some()));
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
        // Live playhead: drag or click to scrub while the cue plays.
        if let Some(elapsed) = playhead {
            match cue.duration().filter(|d| !d.is_zero()) {
                Some(d) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(elapsed.to_clock_string()).font(theme::clock_font(16.0)).color(theme::active()));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format!("-{}", (d - elapsed).to_clock_string())).font(theme::clock_font(16.0)).color(theme::TEXT_DIM));
                        });
                    });
                    seek_to = scrub_bar(ui, ("insp-scrub", id), elapsed, d, 22.0, theme::active());
                    ui.label(RichText::new("Drag the playhead to seek, even while playing.").size(12.0).color(theme::TEXT_DIM));
                }
                None => {
                    ui.label(RichText::new(format!("Running {}", elapsed.to_clock_string())).font(theme::clock_font(16.0)).color(theme::active()));
                }
            }
            ui.add_space(4.0);
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
            if top_level {
                ui.label("Timecode");
                ui.horizontal(|ui| {
                    let mut on = h.timecode.is_some();
                    if ui.checkbox(&mut on, "").on_hover_text("Fire when show timecode reaches this").changed() {
                        h.timecode = on.then(|| h.timecode.unwrap_or_else(|| livestage_core::Timecode::new(1, 0, 0, 0, tc_rate).unwrap_or_default()));
                        changed = true;
                    }
                    match &mut h.timecode {
                        Some(t) => {
                            changed |= super::widgets::timecode_field(ui, ("cue-tc", id), t, tc_rate);
                            if !list_tc {
                                ui.label(RichText::new("list not in timecode mode").size(12.0).color(theme::standby()));
                            }
                        }
                        None => {
                            ui.weak("manual GO");
                        }
                    }
                });
                ui.end_row();
            }
        });
        ui.separator();
        changed |= match cue {
            AnyCue::Audio(a) => audio(ui, a, &stems, &assets),
            AnyCue::Video(v) => video(ui, v, &outputs, &assets),
            AnyCue::Timer(t) => timer(ui, t, &outputs),
            AnyCue::Dmx(d) => dmx(ui, d, &lighting, &fixture_names),
            AnyCue::Control(c) => control(ui, c),
            AnyCue::Slides(sl) => slides(ui, sl, &outputs, &root, st, &mut slide_nav),
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
    if let Some(t) = seek_to {
        rt.seek_cue(id, t);
    }
    if let Some(nav) = slide_nav {
        rt.slide_nav(Some(id), nav);
    }
}

fn slides(
    ui: &mut Ui,
    s: &mut SlideCue,
    outputs: &[(OutputId, String)],
    root: &std::path::Path,
    st: &mut UiState,
    nav: &mut Option<SlideNav>,
) -> bool {
    let mut c = false;
    let n = s.slide_count();
    let running = s.header.runtime.state.is_active();
    egui::Grid::new("slides").num_columns(2).show(ui, |ui| {
        ui.label("Output");
        c |= output_combo(ui, "sout", &mut s.output, outputs);
        ui.end_row();
        ui.label("Layer");
        c |= ui.add(DragValue::new(&mut s.layer)).changed();
        ui.end_row();
        ui.label("Opacity");
        c |= ui.add(egui::Slider::new(&mut s.opacity, 0.0..=1.0)).changed();
        ui.end_row();
        ui.label("Fade in / out");
        ui.horizontal(|ui| {
            c |= time_edit(ui, &mut s.fade_in);
            c |= time_edit(ui, &mut s.fade_out);
        });
        ui.end_row();
        ui.label("Slide transition");
        c |= time_edit(ui, &mut s.transition);
        ui.end_row();
        ui.label("Start at slide");
        let mut first = s.start_slide + 1;
        if ui.add(DragValue::new(&mut first).range(1..=n.max(1))).changed() {
            s.start_slide = first - 1;
            c = true;
        }
        ui.end_row();
    });
    let source = s.deck.source.as_ref().map_or("—".to_string(), |a| a.file_name().to_string());
    ui.label(RichText::new(format!("{n} slides · {}×{} · {source}", s.deck.size[0], s.deck.size[1])).color(theme::TEXT_DIM));
    if n == 0 {
        ui.label(RichText::new("Import a PDF or PowerPoint file (File ▸ Import, or drop it on the window).").color(theme::TEXT_DIM));
        return c;
    }
    ui.separator();
    if running {
        ui.horizontal(|ui| {
            if ui.add_enabled(s.current > 0, egui::Button::new(RichText::new("◀  Previous").size(15.0)).min_size(egui::vec2(120.0, 34.0))).clicked() {
                *nav = Some(SlideNav::Previous);
            }
            ui.label(RichText::new(format!("{} / {n}", s.current + 1)).font(theme::clock_font(22.0)).color(theme::active()));
            if ui.add_enabled(s.current + 1 < n, egui::Button::new(RichText::new("Next  ▶").size(15.0)).min_size(egui::vec2(120.0, 34.0))).clicked() {
                *nav = Some(SlideNav::Next);
            }
        });
        ui.label(RichText::new("Right / Left arrow keys (or a clicker's PgDn / PgUp) step the running presentation.").size(12.0).color(theme::TEXT_DIM));
    }
    // Thumbnail strip: click to jump (running) or to choose the first slide.
    let marked = if running { s.current } else { s.start_slide };
    let thumb = egui::vec2(150.0, 84.0);
    egui::ScrollArea::vertical().id_salt("slide-strip").max_height(330.0).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            for (i, slide) in s.deck.slides.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(thumb + egui::vec2(0.0, 18.0), egui::Sense::click());
                let pic = egui::Rect::from_min_size(rect.min, thumb);
                let p = ui.painter_at(rect);
                p.rect_filled(pic, 3.0, egui::Color32::BLACK);
                if let Ok(path) = livestage_core::bundle::resolve_asset(root, &slide.image) {
                    if let Some(tex) = st.thumbs.get(&path, ShowTime::ZERO) {
                        let sz = tex.size_vec2();
                        let scale = (pic.width() / sz.x).min(pic.height() / sz.y);
                        let r = egui::Rect::from_center_size(pic.center(), sz * scale);
                        p.image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    }
                }
                let border = if i == marked { theme::active() } else if resp.hovered() { theme::TEXT_DIM } else { theme::BORDER };
                p.rect_stroke(pic, 3.0, egui::Stroke::new(if i == marked { 2.0_f32 } else { 1.0 }, border), egui::StrokeKind::Inside);
                p.text(egui::pos2(pic.left(), pic.bottom() + 2.0), egui::Align2::LEFT_TOP, format!("{}", i + 1), egui::FontId::proportional(12.0), theme::TEXT_DIM);
                let first_line = slide.text.lines().next().unwrap_or_default();
                p.text(
                    egui::pos2(pic.left() + 22.0, pic.bottom() + 2.0),
                    egui::Align2::LEFT_TOP,
                    first_line.chars().take(22).collect::<String>(),
                    egui::FontId::proportional(12.0),
                    theme::TEXT_DIM,
                );
                let resp = if slide.text.is_empty() && slide.notes.is_empty() {
                    resp
                } else {
                    resp.on_hover_text(if slide.notes.is_empty() { slide.text.clone() } else { format!("{}\n\nNotes: {}", slide.text, slide.notes) })
                };
                if resp.clicked() {
                    if running {
                        *nav = Some(SlideNav::Goto(i));
                    } else {
                        s.start_slide = i;
                        c = true;
                    }
                }
            }
        });
    });
    if let Some(slide) = s.deck.slides.get(marked) {
        if !slide.notes.is_empty() {
            ui.label(RichText::new("Speaker notes").strong());
            ui.label(&slide.notes);
        }
    }
    c
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
        VisualContent::Slides { .. } => "Slides",
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
        VisualContent::Slides { slides, .. } => {
            ui.label(RichText::new(format!("{} slides — use a Slides cue to navigate them.", slides.len())).color(theme::TEXT_DIM));
        }
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

fn dmx(ui: &mut Ui, d: &mut DmxCue, lighting: &livestage_core::lighting::LightingConfig, fixtures: &[(livestage_core::id::FixtureId, String)]) -> bool {
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
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} fixture attribute values", d.attributes.len())).color(theme::TEXT_DIM));
            if ui.small_button("Clear").clicked() {
                d.attributes.clear();
                c = true;
            }
        });
    }
    // Palette references: resolved when the cue runs, so palette edits apply.
    if !d.palettes.is_empty() || !lighting.palettes.is_empty() {
        ui.label(RichText::new("Palettes").strong());
        let mut remove = None;
        for (i, r) in d.palettes.iter().enumerate() {
            ui.horizontal(|ui| {
                let name = lighting.palette(r.palette).map_or("(deleted palette)".to_string(), |p| format!("{} · {}", p.kind.label(), p.name));
                let who = match r.fixtures.as_slice() {
                    [one] => fixtures.iter().find(|f| f.0 == *one).map_or("1 fixture".into(), |f| f.1.clone()),
                    many => format!("{} fixtures", many.len()),
                };
                ui.label(name);
                ui.weak(format!("on {who}"));
                if ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            d.palettes.remove(i);
            c = true;
        }
    }
    if !d.effects.is_empty() || !lighting.effects.is_empty() {
        ui.label(RichText::new("Effects started with this look").strong());
        ui.horizontal_wrapped(|ui| {
            for e in &lighting.effects {
                let mut on = d.effects.contains(&e.id);
                if ui.checkbox(&mut on, &e.name).changed() {
                    if on {
                        d.effects.push(e.id);
                    } else {
                        d.effects.retain(|x| *x != e.id);
                    }
                    c = true;
                }
            }
        });
    }
    ui.label(RichText::new("Record looks from the Lighting tab (Record cue / Update cue).").size(12.0).color(theme::TEXT_DIM));
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
    use livestage_core::timecode::{OnTimecodeStop, TimecodeSlave};
    let mut c = false;
    ui.horizontal(|ui| {
        ui.label("Length");
        c |= opt_time_edit(ui, &mut t.length, "from master");
    });
    // Timecode slave
    let rate = p.timecode.rate;
    let mut slaved = t.timecode.is_some();
    if ui
        .checkbox(&mut slaved, RichText::new("Timecode slave").strong())
        .on_hover_text("Run whenever show timecode is inside this timeline, instead of on GO")
        .changed()
    {
        t.timecode = slaved.then(|| TimecodeSlave::new(livestage_core::Timecode::new(1, 0, 0, 0, rate).unwrap_or_default()));
        c = true;
    }
    if let Some(slave) = &mut t.timecode {
        egui::Grid::new("tl-tc").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Starts at");
            ui.horizontal(|ui| {
                c |= super::widgets::timecode_field(ui, ("tl-tc-start", t.header.id), &mut slave.start, rate);
                if let Some(len) = t.length.or_else(|| t.master.as_ref().and_then(|m| m.duration())) {
                    let end = livestage_core::Timecode::from_time(slave.start.with_rate(rate).to_time() + len, rate);
                    ui.weak(format!("→ ends {end}"));
                }
            });
            ui.end_row();
            ui.label("When TC stops");
            ComboBox::from_id_salt("tl-tc-stop").selected_text(slave.on_stop.label()).show_ui(ui, |ui| {
                for o in OnTimecodeStop::ALL {
                    c |= ui.selectable_value(&mut slave.on_stop, o, o.label()).changed();
                }
            });
            ui.end_row();
            ui.label("Re-sync beyond");
            let mut ms = slave.tolerance.as_micros() / 1000;
            if ui.add(egui::DragValue::new(&mut ms).range(5..=2000).suffix(" ms")).changed() {
                slave.tolerance = ShowTime::from_millis(ms);
                c = true;
            }
            ui.end_row();
        });
        ui.label(
            RichText::new(format!(
                "Follows {} · drift corrections so far: {}",
                p.timecode.source.label(),
                slave.follower.resyncs
            ))
            .size(12.0)
            .color(theme::TEXT_DIM),
        );
    }
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
