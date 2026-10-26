//! DMX engine: executes core `DmxAction`s, renders playback through the
//! lighting console layers, and pixel maps.

use crate::console::{resolve_palettes, Console, Layer, Slot, SlotValue};
use crate::fixture::{FixtureLibrary, Patch};
use crate::output::{DmxOutput, SharedFrame};
use crate::pixelmap::{self, RgbaView};
use crate::playback::DmxPlayback;
use crate::sacn::SacnSource;
use crate::sacn_in::{self, SacnReceiver};
use crate::universe::Universe;
use livestage_core::action::DmxAction;
use livestage_core::dmx_config::DmxConfig;
use livestage_core::id::{FixtureId, OutputId};
use livestage_core::lighting::LightingConfig;
use livestage_core::time::ShowTime;
use std::collections::BTreeMap;

pub struct DmxEngine {
    config: DmxConfig,
    pub library: FixtureLibrary,
    patch: Patch,
    pub patch_errors: Vec<String>,
    playback: DmxPlayback,
    universes: BTreeMap<u16, Universe>,
    frame: SharedFrame,
    output: Option<DmxOutput>,
    /// This LiveStage on sACN (its CID keeps our output out of our input).
    source: SacnSource,
    input: Option<SacnReceiver>,
    /// Input generation the last render merged.
    input_seen: u64,
    pixel_enabled: Vec<bool>,
    /// Programmer, effects, flashes and blackout.
    pub console: Console,
    /// Home value of every patched attribute, built with the patch.
    home: Layer,
    /// Render buffer, reused; after a render it holds the final value of
    /// every slot (read back by the UI through [`DmxEngine::output`]).
    layer: Layer,
    render_period: ShowTime,
    last_render: Option<ShowTime>,
    /// Render on the next tick regardless of the rate (a cue just fired).
    force_render: bool,
    renders: u64,
}

impl DmxEngine {
    pub fn new(config: DmxConfig) -> Self {
        let (library, mut patch_errors) = FixtureLibrary::with_profiles(&config.profiles);
        let (patch, errors) = Patch::build(&config.patch, &library);
        patch_errors.extend(errors);
        for e in &patch_errors {
            tracing::warn!("patch: {e}");
        }
        let universes = config.universes.iter().map(|u| (u.index, Universe::default())).collect();
        let pixel_enabled = config.pixel_maps.iter().map(|m| m.enabled).collect();
        let mut home = Layer::default();
        for (_, r) in patch.fixtures().flat_map(|(f, _)| patch.attributes_of(f.id)) {
            home.insert(r.slot(), SlotValue { value: r.default, fine: r.fine, kind: r.kind });
        }
        let render_hz = config.render_hz.clamp(config.refresh_hz.clamp(1.0, 44.0), 200.0);
        Self {
            config,
            library,
            patch,
            patch_errors,
            playback: DmxPlayback::default(),
            universes,
            frame: Default::default(),
            output: None,
            source: SacnSource::new("LiveStage"),
            input: None,
            input_seen: 0,
            pixel_enabled,
            console: Console::default(),
            layer: home.clone(),
            home,
            render_period: ShowTime::from_secs_f64(1.0 / render_hz as f64),
            last_render: None,
            force_render: false,
            renders: 0,
        }
    }

    pub fn patch(&self) -> &Patch {
        &self.patch
    }

    /// Starts network output, and sACN input when it is enabled and patched.
    /// Failures are logged and leave the engine usable offline.
    pub fn start_output(&mut self) {
        match DmxOutput::start(&self.config, self.frame.clone(), self.source.clone()) {
            Ok(o) => self.output = Some(o),
            Err(e) => tracing::error!("DMX output unavailable: {e}"),
        }
        let input = &self.config.sacn_input;
        let mut universes: Vec<u16> = input.patches.iter().filter(|p| p.enabled).map(|p| p.sacn_universe).collect();
        universes.sort_unstable();
        universes.dedup();
        if input.enabled && !universes.is_empty() {
            match SacnReceiver::start(&universes, self.source.cid) {
                Ok(r) => self.input = Some(r),
                Err(e) => tracing::error!("sACN input unavailable: {e}"),
            }
        }
    }

    /// Received sACN universes with live sources: (universe, sources, priority).
    pub fn input_status(&self) -> Vec<(u16, u8, u8)> {
        let Some(input) = &self.input else { return Vec::new() };
        let mut v: Vec<_> = input.shared.universes.lock().iter().map(|(u, m)| (*u, m.sources, m.priority)).collect();
        v.sort_unstable();
        v
    }

    /// sACN input is running.
    pub fn input_running(&self) -> bool {
        self.input.is_some()
    }

    pub fn handle(&mut self, now: ShowTime, action: &DmxAction, lighting: &LightingConfig) {
        self.force_render = true;
        match action {
            DmxAction::ApplyScene { cue, channels, attributes, palettes, effects, fade } => {
                // Palette values first; the cue's own attributes override them.
                let mut values = resolve_palettes(palettes, lighting, &self.patch);
                values.extend(attributes.iter().cloned());
                self.playback.apply_scene(now, *cue, channels, &values, *fade, &self.patch);
                self.console.cue_started(*cue, effects, attributes, palettes, now);
            }
            DmxAction::Release { cue, fade } => {
                self.playback.release(now, *cue, *fade);
                self.console.cue_released(*cue);
            }
            DmxAction::Blackout { fade } => {
                // Hard panic: nothing of the console may keep a light on.
                self.playback.blackout(now, *fade);
                self.console.stop_effects();
                self.console.programmer.clear();
            }
            DmxAction::SetPixelMap { index, enabled } => {
                if let Some(e) = self.pixel_enabled.get_mut(*index) {
                    *e = *enabled;
                }
            }
        }
    }

    /// Feeds a rendered canvas to every pixel map sampling `output`.
    pub fn feed_canvas(&mut self, output: OutputId, width: u32, height: u32, rgba: &[u8]) {
        let view = RgbaView { width, height, data: rgba };
        for (i, map) in self.config.pixel_maps.iter().enumerate() {
            if map.source == output && self.pixel_enabled.get(i).copied().unwrap_or(false) {
                pixelmap::render(map, view, &mut self.universes);
            }
        }
    }

    /// Re-resolves looks on stage that use palettes, after a palette edit.
    pub fn palettes_changed(&mut self, now: ShowTime, lighting: &LightingConfig) {
        self.force_render = true;
        let scenes: Vec<_> = self.console.palette_scenes().map(|(c, (a, p))| (*c, a.clone(), p.clone())).collect();
        for (cue, attributes, palettes) in scenes {
            let mut values = resolve_palettes(&palettes, lighting, &self.patch);
            values.extend(attributes);
            self.playback.apply_scene(now, cue, &[], &values, ShowTime::from_millis(300), &self.patch);
        }
    }

    /// Called every show-thread tick. Renders at most at `render_hz`
    /// (default 100 Hz; never below the transmit rate), plus right after a
    /// cue action so GO latency is one show tick. Returns whether it rendered.
    pub fn tick(&mut self, now: ShowTime, lighting: &LightingConfig) -> bool {
        let due = match self.last_render {
            None => true,
            // `now < t`: show time jumped back (a new epoch); render at once.
            Some(t) => self.force_render || now < t || now >= t + self.render_period || self.input_changed(),
        };
        if due {
            self.render(now, lighting);
        }
        due
    }

    fn input_changed(&self) -> bool {
        self.input.as_ref().is_some_and(|i| i.shared.generation.load(std::sync::atomic::Ordering::Acquire) != self.input_seen)
    }

    /// Renders home values, playback and the console layers into the
    /// universes and publishes changed universes to the sender.
    /// Allocation-free once the layer planes and frame exist.
    pub fn render(&mut self, now: ShowTime, lighting: &LightingConfig) {
        self.last_render = Some(now);
        self.force_render = false;
        self.renders += 1;
        self.layer.reset_from(&self.home);
        for (slot, value, fine, kind) in self.playback.values(now) {
            self.layer.insert(slot, SlotValue { value, fine, kind });
        }
        self.console.apply(&mut self.layer, now, lighting, &self.patch);
        for (index, plane) in self.layer.planes() {
            let Some(u) = self.universes.get_mut(&index) else { continue };
            for (i, v) in plane.iter().enumerate() {
                let Some(v) = v else { continue };
                let channel = i as u16 + 1;
                match v.fine {
                    Some(fine) => {
                        let v16 = (v.value.clamp(0.0, 1.0) * 65_535.0).round() as u16;
                        u.set(channel, (v16 >> 8) as u8);
                        u.set(fine, (v16 & 0xFF) as u8);
                    }
                    None => u.set(channel, (v.value.clamp(0.0, 1.0) * 255.0).round() as u8),
                }
            }
        }
        // Received sACN is merged into what is sent, not into the engine's
        // own universes, so input that stops leaves nothing behind.
        let input = self.input.as_ref().map(|i| {
            self.input_seen = i.shared.generation.load(std::sync::atomic::Ordering::Acquire);
            i.shared.universes.lock()
        });
        let patches = &self.config.sacn_input.patches;
        let mut f = self.frame.lock();
        for (index, u) in &self.universes {
            let mut data = u.data;
            if let Some(input) = &input {
                sacn_in::apply_patches(patches, *index, input, &mut data);
            }
            match f.get_mut(index) {
                Some(sent) if sent.data == data => {}
                Some(sent) => sent.data = data,
                None => {
                    f.insert(*index, Universe { data });
                }
            }
        }
    }

    /// Number of renders so far (diagnostics and tests).
    pub fn renders(&self) -> u64 {
        self.renders
    }

    /// Interval between renders.
    pub fn render_period(&self) -> ShowTime {
        self.render_period
    }

    /// Output value (0..=1) of a fixture attribute after the last render
    /// (tests; the UI reads its frame copy, `DmxView::output`).
    #[cfg(test)]
    pub(crate) fn output(&self, fixture: FixtureId, attribute: &str) -> Option<f32> {
        let r = self.patch.resolve(fixture, attribute)?;
        self.layer.get(r.slot()).map(|v| v.value)
    }

    /// Every slot value written by the last render (for UI snapshots).
    pub fn last_values(&self) -> &Layer {
        &self.layer
    }

    pub fn universe(&self, index: u16) -> Option<&Universe> {
        self.universes.get(&index)
    }
    pub fn config(&self) -> &DmxConfig {
        &self.config
    }
    pub fn packets_sent(&self) -> u64 {
        self.output.as_ref().map_or(0, |o| o.packets_sent.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// Approximate emitted colour (0..=1 RGB, intensity applied) of a fixture,
/// given the output value of a slot. Uses the colour slots the patch
/// resolved when it was built: no attribute lookups per call.
pub fn fixture_color(patch: &Patch, fixture: FixtureId, get: impl Fn(Slot) -> Option<f32>) -> Option<[f32; 3]> {
    let slots = patch.color_slots(fixture)?;
    let v = |s: Option<Slot>| s.and_then(&get);
    let dimmer = if slots.intensity.is_some() { v(slots.intensity).unwrap_or(0.0) } else { 1.0 };
    let mut c = if let Some(red) = v(slots.red) {
        let w = v(slots.white).unwrap_or(0.0) + v(slots.amber).unwrap_or(0.0) * 0.5;
        [red + w, v(slots.green).unwrap_or(0.0) + w * 0.8, v(slots.blue).unwrap_or(0.0) + w * 0.6]
    } else {
        [1.0, 0.95, 0.85]
    };
    for v in &mut c {
        *v = (*v * dimmer).clamp(0.0, 1.0);
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestage_core::action::FixtureAttributeValue;
    use livestage_core::dmx_config::{PatchedFixture, PixelMapConfig, PixelOrder, PixelWiring, UniverseConfig};
    use livestage_core::id::{CueId, OutputId};
    use livestage_core::lighting::{Effect, EffectKind, LightValue, Submaster, SubmasterKind};

    /// `n` 12-channel moving heads, 42 per universe.
    fn heads(n: usize) -> (DmxConfig, Vec<FixtureId>) {
        let patch: Vec<PatchedFixture> = (0..n)
            .map(|i| PatchedFixture {
                id: FixtureId::new(),
                name: format!("Spot {}", i + 1),
                definition: "generic/moving-head".into(),
                mode: "12ch".into(),
                universe: (i / 42) as u16,
                address: (i % 42) as u16 * 12 + 1,
            })
            .collect();
        let ids = patch.iter().map(|f| f.id).collect();
        let universes = (0..n.div_ceil(42) as u16)
            .map(|index| UniverseConfig { index, name: format!("U{index}"), transport: livestage_core::dmx_config::DmxTransport::Disabled })
            .collect();
        (DmxConfig { universes, patch, ..DmxConfig::default() }, ids)
    }

    #[test]
    fn renders_at_bounded_rate_but_cues_render_at_once() {
        let (cfg, ids) = heads(2);
        let lighting = LightingConfig::default();
        let mut dmx = DmxEngine::new(cfg);
        // One second of 1 kHz show ticks: 100 renders at the default 100 Hz.
        for ms in 0..1_000 {
            dmx.tick(ShowTime::from_millis(ms), &lighting);
        }
        assert_eq!(dmx.renders(), 100);
        // A cue fired between renders reaches the output on the next show tick.
        let attributes = vec![FixtureAttributeValue { fixture: ids[0], attribute: "intensity".into(), value: 1.0 }];
        let go = DmxAction::ApplyScene { cue: CueId::new(), channels: vec![], attributes, palettes: vec![], effects: vec![], fade: ShowTime::ZERO };
        dmx.handle(ShowTime::from_millis(1_001), &go, &lighting);
        assert!(dmx.tick(ShowTime::from_millis(1_001), &lighting));
        assert_eq!(dmx.output(ids[0], "intensity"), Some(1.0));
        assert_eq!(dmx.frame.lock()[&0].get(5), 255, "published to the sender");
        assert!(!dmx.tick(ShowTime::from_millis(1_002), &lighting));
        assert_eq!(dmx.output(ids[0], "intensity"), Some(1.0), "readback holds between renders");
    }

    #[test]
    fn render_rate_never_below_transmit_rate() {
        let cfg = DmxConfig { render_hz: 5.0, refresh_hz: 40.0, ..DmxConfig::default() };
        assert_eq!(DmxEngine::new(cfg).render_period(), ShowTime::from_micros(25_000));
        let cfg = DmxConfig { render_hz: 10_000.0, ..DmxConfig::default() };
        assert_eq!(DmxEngine::new(cfg).render_period(), ShowTime::from_micros(5_000));
    }

    #[test]
    fn pixel_maps_are_published_on_the_next_render() {
        let source = OutputId::new();
        let map = PixelMapConfig {
            name: "Strip".into(),
            source,
            region: [0.0, 0.0, 1.0, 1.0],
            width: 2,
            height: 1,
            order: PixelOrder::Rgb,
            wiring: PixelWiring::RowsLeftToRight,
            start_universe: 0,
            start_address: 100,
            enabled: true,
        };
        let lighting = LightingConfig::default();
        let mut dmx = DmxEngine::new(DmxConfig { pixel_maps: vec![map], ..DmxConfig::default() });
        dmx.tick(ShowTime::ZERO, &lighting);
        dmx.feed_canvas(source, 2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
        dmx.tick(ShowTime::from_millis(10), &lighting);
        let f = dmx.frame.lock();
        assert_eq!((f[&0].get(100), f[&0].get(105)), (255, 255));
    }

    /// The layered render for a large rig. Run in release:
    /// `cargo test -p livestage_dmx --release -- --ignored --nocapture render_budget`
    /// Two effects over every head, a look and an inhibit submaster, the
    /// programmer, the grand master and a 5 s crossfade on 2 048 slots:
    /// ≈ 60 µs per render on an M-series Mac (was ≈ 960 µs, on every
    /// 1 kHz tick), i.e. ≈ 6 µs per show tick on average at 100 Hz.
    #[test]
    #[ignore = "benchmark; run in release"]
    fn render_budget_512_moving_heads() {
        let (cfg, ids) = heads(512);
        let mut lighting = LightingConfig::default();
        let mut circle = Effect::new("Circle", EffectKind::Circle, ids.iter().step_by(2).copied().collect());
        circle.size = 0.3;
        let wave = Effect::new("Wave", EffectKind::DimmerChase, ids.iter().skip(1).step_by(2).copied().collect());
        let (circle_id, wave_id) = (circle.id, wave.id);
        lighting.effects = vec![circle, wave];
        let mut sub = Submaster::new("Inhibit", SubmasterKind::Inhibit, ids[..256].to_vec());
        sub.level = 0.8;
        let mut look = Submaster::new("Blinders", SubmasterKind::Additive { values: vec![LightValue::any("intensity", 1.0)] }, ids[256..].to_vec());
        look.level = 0.5;
        lighting.submasters = vec![sub, look];
        lighting.grand_master = 0.9;

        let mut dmx = DmxEngine::new(cfg);
        let attributes = ids
            .iter()
            .flat_map(|f| ["intensity", "pan", "tilt", "zoom"].map(|a| FixtureAttributeValue { fixture: *f, attribute: a.into(), value: 0.6 }))
            .collect();
        let go = DmxAction::ApplyScene { cue: CueId::new(), channels: vec![], attributes, palettes: vec![], effects: vec![], fade: ShowTime::from_secs(5) };
        dmx.handle(ShowTime::ZERO, &go, &lighting);
        dmx.console.set_effect(circle_id, true, ShowTime::ZERO);
        dmx.console.set_effect(wave_id, true, ShowTime::ZERO);
        dmx.console.programmer.selection = ids[..64].to_vec();
        let patch = dmx.patch().clone();
        dmx.console.programmer.set_selected("color", 0.3, &patch);

        let n = 2_000u64;
        for i in 0..50 {
            dmx.render(ShowTime::from_millis(i), &lighting); // warm-up: planes and frame allocated
        }
        let start = std::time::Instant::now();
        for i in 0..n {
            dmx.render(ShowTime::from_millis(50 + i), &lighting);
        }
        let per_render = start.elapsed() / n as u32;
        // Amortised over the 1 kHz show thread at the render rate.
        let renders_per_tick = 1_000.0 / dmx.render_period().as_micros() as f64; // period in µs, ticks every 1 000 µs
        let per_tick = per_render.as_secs_f64() * 1e6 * renders_per_tick;
        println!("512 heads: {per_render:?} per render, ≈ {per_tick:.1} µs per 1 kHz show tick on average");
        if !cfg!(debug_assertions) {
            assert!(per_render < std::time::Duration::from_micros(100), "render took {per_render:?}");
        }
    }
}
