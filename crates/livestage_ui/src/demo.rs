//! Cue constructors and the demo show used on first launch.

use livestage_core::action::*;
use livestage_core::cue::*;
use livestage_core::project::{AssetRef, Project};
use livestage_core::timeline::TimelineCue;
use livestage_core::{CoreResult, ProjectBundle, ShowTime};
use std::path::Path;

pub fn audio_cue(p: &Project, number: &str, name: &str, asset: AssetRef, media_length: Option<ShowTime>) -> AnyCue {
    let stem = p.audio.stems.first().map(|s| s.id).unwrap_or_default();
    AnyCue::Audio(AudioCue {
        header: CueHeader::new(number, name),
        tracks: vec![AudioTrack { asset, stem, gain_db: 0.0, mute: false }],
        click: None,
        gain_db: 0.0,
        fade_in: ShowTime::ZERO,
        fade_out: ShowTime::from_millis(1_000),
        start_at: ShowTime::ZERO,
        end_at: None,
        looped: false,
        media_length,
    })
}

pub fn visual_cue(p: &Project, number: &str, name: &str, asset: AssetRef, video: bool) -> AnyCue {
    let output = p.displays.first().map(|d| d.id).unwrap_or_default();
    AnyCue::Video(VideoCue {
        header: CueHeader::new(number, name),
        output,
        content: if video { VisualContent::Video { asset, looped: false } } else { VisualContent::Image { asset } },
        layer: 0,
        opacity: 1.0,
        fade_in: ShowTime::from_millis(500),
        fade_out: ShowTime::from_millis(500),
        hold: None,
        media_length: None,
        end_behavior: EndBehavior::Hide,
    })
}

pub fn slide_cue(p: &Project, number: &str, name: &str, deck: SlideDeck) -> AnyCue {
    // Presentations go to the confidence / second screen if there is one,
    // else the first output.
    let output = p.displays.first().map(|d| d.id).unwrap_or_default();
    AnyCue::Slides(SlideCue {
        header: CueHeader::new(number, name),
        output,
        deck,
        start_slide: 0,
        layer: 5,
        opacity: 1.0,
        fade_in: ShowTime::from_millis(300),
        fade_out: ShowTime::from_millis(300),
        transition: ShowTime::from_millis(250),
        current: 0,
    })
}

pub fn new_cue(p: &Project, kind: CueKind, number: &str) -> AnyCue {
    let output = p.displays.first().map(|d| d.id).unwrap_or_default();
    let h = CueHeader::new(number, format!("New {} cue", kind.label()));
    match kind {
        CueKind::Audio => AnyCue::Audio(AudioCue {
            header: h,
            tracks: vec![],
            click: Some(ClickTrack {
                bpm: 120.0,
                beats_per_bar: 4,
                stem: p.audio.stem_by_name("Click").map(|s| s.id).unwrap_or_default(),
                gain_db: -6.0,
                count_in_bars: 0,
            }),
            gain_db: 0.0,
            fade_in: ShowTime::ZERO,
            fade_out: ShowTime::ZERO,
            start_at: ShowTime::ZERO,
            end_at: None,
            looped: false,
            media_length: None,
        }),
        CueKind::Video => AnyCue::Video(VideoCue {
            header: h,
            output,
            content: VisualContent::TestPattern,
            layer: 0,
            opacity: 1.0,
            fade_in: ShowTime::from_millis(500),
            fade_out: ShowTime::from_millis(500),
            hold: None,
            media_length: None,
            end_behavior: EndBehavior::Hide,
        }),
        CueKind::Timer => timer_cue(output, number, "Countdown", ShowTime::from_secs(300)),
        CueKind::Dmx => AnyCue::Dmx(DmxCue {
            header: h,
            channels: vec![DmxChannelValue { universe: 0, channel: 1, value: 255 }],
            attributes: vec![],
            fade: ShowTime::from_secs(2),
            release_on_stop: true,
            palettes: vec![],
            effects: vec![],
        }),
        CueKind::Control => AnyCue::Control(ControlCue {
            header: h,
            action: ControlAction::SendOsc { host: "127.0.0.1".into(), port: 53000, address: "/go".into(), args: vec![] },
        }),
        CueKind::Timeline => AnyCue::Timeline(TimelineCue::new(h, None)),
        CueKind::Slides => {
            let mut c = slide_cue(p, number, "New presentation (import a PDF / PowerPoint)", SlideDeck::default());
            c.header_mut().name = h.name;
            c
        }
    }
}

pub fn timer_cue(output: livestage_core::OutputId, number: &str, label: &str, from: ShowTime) -> AnyCue {
    AnyCue::Timer(TimerCue {
        header: CueHeader::new(number, label),
        output,
        timer: TimerContent {
            mode: TimerMode::CountDown { from },
            label: label.to_uppercase(),
            font_size: 220.0,
            color: [0.95, 0.95, 0.95, 1.0],
            warn_color: [0.898, 0.224, 0.208, 1.0],
            warn_below: ShowTime::from_secs(30),
            started_at: ShowTime::ZERO,
            elapsed_offset: ShowTime::ZERO,
        },
        layer: 10,
        hold_at_zero: true,
    })
}

fn dmx_look(number: &str, name: &str, values: &[(u16, u8)], fade_ms: u64) -> AnyCue {
    AnyCue::Dmx(DmxCue {
        header: CueHeader::new(number, name),
        channels: values.iter().map(|&(channel, value)| DmxChannelValue { universe: 0, channel, value }).collect(),
        attributes: vec![],
        fade: ShowTime::from_millis(fade_ms),
        release_on_stop: true,
        palettes: vec![],
        effects: vec![],
    })
}

/// Writes a 16-bit stereo WAV with a soft chord (demo backing track).
fn write_demo_wav(path: &Path, secs: u32) -> std::io::Result<()> {
    let rate = 48_000u32;
    let frames = rate * secs;
    let mut data = Vec::with_capacity(frames as usize * 4);
    let freqs = [220.0f32, 277.18, 329.63];
    for f in 0..frames {
        let t = f as f32 / rate as f32;
        let env = (t * 2.0).min(1.0) * ((secs as f32 - t) * 2.0).clamp(0.0, 1.0);
        let pulse = 0.6 + 0.4 * (t * std::f32::consts::TAU * 2.0).sin().abs();
        let s: f32 = freqs.iter().map(|fr| (t * fr * std::f32::consts::TAU).sin()).sum::<f32>() / 3.0;
        let v = (s * env * pulse * 0.3 * i16::MAX as f32) as i16;
        data.extend_from_slice(&v.to_le_bytes());
        data.extend_from_slice(&v.to_le_bytes());
    }
    let mut b = Vec::with_capacity(44 + data.len());
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    std::fs::write(path, b)
}

/// Creates a demo bundle exercising every cue type and a stacked timeline.
pub fn create_demo(parent: &Path) -> CoreResult<ProjectBundle> {
    let mut bundle = ProjectBundle::create(parent, "Demo Show")?;
    let tmp = std::env::temp_dir().join("LiveStage Demo Backing.wav");
    write_demo_wav(&tmp, 24).map_err(|e| livestage_core::CoreError::Io { path: tmp.clone(), source: e })?;
    let rec = bundle.import_asset(&tmp)?;
    let _ = std::fs::remove_file(&tmp);

    let p = &bundle.project;
    let wall = p.displays[0].id;
    let confidence = p.displays.get(1).map_or(wall, |d| d.id);
    let backing = p.audio.stems[0].id;
    let click = p.audio.stem_by_name("Click").map(|s| s.id).unwrap_or(backing);

    let mut cues = Vec::new();
    cues.push(timer_cue(confidence, "1", "Doors", ShowTime::from_secs(300)));
    let mut pattern = new_cue(p, CueKind::Video, "2");
    pattern.header_mut().name = "Line-up test pattern".into();
    cues.push(pattern);
    cues.push(dmx_look("3", "Preset: house half", &[(1, 128), (2, 128)], 3_000));

    // Stacked timeline: the song's sample clock drives looks, a lyric and a timer.
    let master = AnyCue::Audio(AudioCue {
        header: CueHeader::new("4.0", "Backing track"),
        tracks: vec![AudioTrack { asset: rec.path.clone(), stem: backing, gain_db: -3.0, mute: false }],
        click: Some(ClickTrack { bpm: 120.0, beats_per_bar: 4, stem: click, gain_db: -10.0, count_in_bars: 0 }),
        gain_db: 0.0,
        fade_in: ShowTime::ZERO,
        fade_out: ShowTime::from_millis(1_500),
        start_at: ShowTime::ZERO,
        end_at: None,
        looped: false,
        media_length: Some(ShowTime::from_secs(24)),
    });
    let mut tl = TimelineCue::new(CueHeader::new("4", "Song 1 — Timeline"), Some(master));
    tl.add(ShowTime::ZERO, dmx_look("4.1", "Verse look", &[(1, 255), (2, 60), (3, 0)], 1_000));
    tl.add(
        ShowTime::from_secs(4),
        AnyCue::Video(VideoCue {
            header: CueHeader::new("4.2", "Lyric: verse 1"),
            output: wall,
            content: VisualContent::Text(TextContent {
                text: "Every light in the house is ours tonight".into(),
                font_size: 72.0,
                color: [1.0, 1.0, 1.0, 1.0],
                scroll_speed: 0.0,
                mirror_horizontal: false,
            }),
            layer: 5,
            opacity: 1.0,
            fade_in: ShowTime::from_millis(300),
            fade_out: ShowTime::from_millis(300),
            hold: Some(ShowTime::from_secs(6)),
            media_length: None,
            end_behavior: EndBehavior::Hide,
        }),
    );
    tl.add(ShowTime::from_secs(8), dmx_look("4.3", "Chorus hit", &[(1, 255), (2, 255), (3, 255)], 200));
    tl.add(ShowTime::from_secs(14), timer_cue(confidence, "4.4", "Song ends in", ShowTime::from_secs(10)));
    tl.add(
        ShowTime::from_secs(16),
        AnyCue::Control(ControlCue {
            header: CueHeader::new("4.5", "Marker: bridge"),
            action: ControlAction::Marker { text: "Bridge reached".into() },
        }),
    );
    cues.push(AnyCue::Timeline(tl));

    let mut teleprompter = AnyCue::Video(VideoCue {
        header: CueHeader::new("5", "Teleprompter: speech"),
        output: confidence,
        content: VisualContent::Text(TextContent {
            text: "Good evening and welcome.\n\nPlease silence your phones.\n\nOur first act needs no introduction…".into(),
            font_size: 64.0,
            color: [1.0, 1.0, 1.0, 1.0],
            scroll_speed: 40.0,
            mirror_horizontal: false,
        }),
        layer: 1,
        opacity: 1.0,
        fade_in: ShowTime::from_millis(500),
        fade_out: ShowTime::from_millis(500),
        hold: None,
        media_length: None,
        end_behavior: EndBehavior::Hide,
    });
    teleprompter.header_mut().notes = "Scrolls at 40 pt/s on the confidence monitor".into();
    cues.push(teleprompter);
    cues.push(AnyCue::Control(ControlCue {
        header: CueHeader::new("6", "Stop everything (2 s)"),
        action: ControlAction::Command(livestage_core::ShowCommand::StopAll { fade: Some(ShowTime::from_secs(2)) }),
    }));

    if let Some(list) = bundle.project.active_list_mut() {
        list.cues = cues;
    }
    bundle.save()?;
    Ok(bundle)
}

/// Patches a small touring rig with palettes, submasters, effects and a
/// stage plot, so the lighting console can be tried without a real patch.
/// Fixtures go on universe 1 after anything already patched.
pub fn lighting_rig(p: &mut Project) {
    use livestage_core::dmx_config::PatchedFixture;
    use livestage_core::id::FixtureId;
    use livestage_core::lighting::{Effect, EffectKind, LightValue, Palette, PaletteKind, Submaster, SubmasterKind};
    use livestage_core::state::Rgb;

    let mut next = p.dmx.patch.iter().filter(|f| f.universe == 0).map(|f| f.address + 12).max().unwrap_or(1);
    let mut add = |p: &mut Project, name: String, def: &str, mode: &str, footprint: u16, at: [f32; 2]| -> FixtureId {
        let id = FixtureId::new();
        p.dmx.patch.push(PatchedFixture { id, name, definition: def.into(), mode: mode.into(), universe: 0, address: next });
        p.lighting.stage.insert(id, at);
        next += footprint;
        id
    };
    let heads: Vec<_> = (0..4).map(|i| add(p, format!("Spot {}", i + 1), "generic/moving-head", "12ch", 12, [0.2 + 0.2 * i as f32, 0.18])).collect();
    let wash: Vec<_> = (0..6).map(|i| add(p, format!("Wash {}", i + 1), "generic/rgbw-par", "5ch", 5, [0.1 + 0.16 * i as f32, 0.72])).collect();
    let floor: Vec<_> = (0..4).map(|i| add(p, format!("Floor {}", i + 1), "generic/rgb-par", "3ch", 3, [0.25 + 0.17 * i as f32, 0.45])).collect();
    let blinders: Vec<_> = (0..2).map(|i| add(p, format!("Blinder {}", i + 1), "generic/dimmer", "8bit", 1, [0.04 + 0.92 * i as f32, 0.5])).collect();
    let pars: Vec<_> = wash.iter().chain(&floor).copied().collect();

    let l = &mut p.lighting;
    let color = |name: &str, c: Rgb, white: f32| {
        let mut pal = Palette::new(
            name,
            PaletteKind::Color,
            vec![
                LightValue::any("red", c.0 as f32 / 255.0),
                LightValue::any("green", c.1 as f32 / 255.0),
                LightValue::any("blue", c.2 as f32 / 255.0),
                LightValue::any("white", white),
            ],
        );
        pal.swatch = Some(c);
        pal
    };
    l.palettes.extend([
        color("Red", Rgb(255, 0, 0), 0.0),
        color("Amber", Rgb(255, 140, 0), 0.0),
        color("Blue", Rgb(0, 40, 255), 0.0),
        color("Magenta", Rgb(255, 0, 200), 0.0),
        color("Cyan", Rgb(0, 220, 255), 0.0),
        color("White", Rgb(255, 255, 255), 1.0),
    ]);
    let full = Palette::new("Full", PaletteKind::Intensity, vec![LightValue::any("intensity", 1.0)]);
    let half = Palette::new("Half", PaletteKind::Intensity, vec![LightValue::any("intensity", 0.5)]);
    l.palettes.extend([full, half]);
    let pos = |name: &str, f: &dyn Fn(usize) -> (f32, f32)| {
        let values = heads.iter().enumerate().flat_map(|(i, h)| {
            let (pan, tilt) = f(i);
            [LightValue::of(*h, "pan", pan), LightValue::of(*h, "tilt", tilt)]
        });
        Palette::new(name, PaletteKind::Position, values.collect())
    };
    l.palettes.extend([
        pos("Centre", &|_| (0.5, 0.5)),
        pos("Audience", &|i| (0.42 + 0.05 * i as f32, 0.82)),
        pos("Cross", &|i| (if i < 2 { 0.65 } else { 0.35 }, 0.62)),
        pos("Drum riser", &|i| (0.5 + 0.04 * (1.5 - i as f32), 0.35)),
    ]);
    l.palettes.extend([
        Palette::new("Open", PaletteKind::Beam, vec![LightValue::any("gobo", 0.0), LightValue::any("zoom", 0.5)]),
        Palette::new("Gobo spin", PaletteKind::Beam, vec![LightValue::any("gobo", 0.6), LightValue::any("zoom", 0.3)]),
        Palette::new("Wide", PaletteKind::Beam, vec![LightValue::any("gobo", 0.0), LightValue::any("zoom", 1.0)]),
    ]);

    let sub = |name: &str, kind: SubmasterKind, fixtures: Vec<FixtureId>, color: Rgb| {
        let mut s = Submaster::new(name, kind, fixtures);
        s.color = Some(color);
        s
    };
    l.submasters.extend([
        sub("Blinders", SubmasterKind::Additive { values: vec![LightValue::any("intensity", 1.0)] }, blinders.clone(), Rgb(255, 214, 120)),
        sub("Strobe", SubmasterKind::Strobe { min_hz: 2.0, max_hz: 18.0 }, vec![], Rgb(240, 240, 240)),
        sub(
            "Warm wash",
            SubmasterKind::Additive {
                values: vec![LightValue::any("intensity", 1.0), LightValue::any("red", 1.0), LightValue::any("green", 0.45), LightValue::any("blue", 0.05)],
            },
            wash.clone(),
            Rgb(255, 140, 40),
        ),
        sub("Heads", SubmasterKind::Inhibit, heads.clone(), Rgb(120, 170, 255)),
        sub("Pars", SubmasterKind::Inhibit, pars.clone(), Rgb(255, 120, 200)),
    ]);

    let fx = |name: &str, kind: EffectKind, fixtures: &[FixtureId], rate: f32, size: f32, spread: f32| {
        let mut e = Effect::new(name, kind, fixtures.to_vec());
        e.rate_hz = rate;
        e.size = size;
        e.spread = spread;
        e
    };
    l.effects.extend([
        fx("Ballyhoo circle", EffectKind::Circle, &heads, 0.2, 0.35, 0.5),
        fx("Tilt wave", EffectKind::Sine { attribute: "tilt".into() }, &heads, 0.3, 0.3, 1.0),
        fx(
            "Rainbow chase",
            EffectKind::ColorChase { colors: vec![Rgb(255, 0, 0), Rgb(255, 200, 0), Rgb(0, 255, 60), Rgb(0, 200, 255), Rgb(40, 0, 255), Rgb(255, 0, 200)] },
            &pars,
            0.25,
            1.0,
            1.0,
        ),
        fx("Dimmer chase", EffectKind::DimmerChase, &wash, 1.0, 1.0, 1.0),
    ]);
}
