//! DMX engine: executes core `DmxAction`s, renders playback through the
//! lighting console layers, and pixel maps.

use crate::console::{resolve_palettes, Console, SlotValue};
use crate::fixture::{FixtureLibrary, Patch};
use crate::output::{DmxOutput, SharedFrame};
use crate::pixelmap::{self, RgbaView};
use crate::playback::DmxPlayback;
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
    pixel_enabled: Vec<bool>,
    /// Programmer, effects, flashes and blackout.
    pub console: Console,
    /// Final value of every slot written by the last tick (for the UI).
    last: BTreeMap<crate::playback::Slot, f32>,
}

impl DmxEngine {
    pub fn new(config: DmxConfig) -> Self {
        let library = FixtureLibrary::builtin();
        let (patch, patch_errors) = Patch::build(&config.patch, &library);
        for e in &patch_errors {
            tracing::warn!("patch: {e}");
        }
        let universes = config.universes.iter().map(|u| (u.index, Universe::default())).collect();
        let pixel_enabled = config.pixel_maps.iter().map(|m| m.enabled).collect();
        Self {
            config,
            library,
            patch,
            patch_errors,
            playback: DmxPlayback::default(),
            universes,
            frame: Default::default(),
            output: None,
            pixel_enabled,
            console: Console::default(),
            last: BTreeMap::new(),
        }
    }

    pub fn patch(&self) -> &Patch {
        &self.patch
    }

    /// Starts network output. Failure is logged and leaves the engine usable offline.
    pub fn start_output(&mut self) {
        match DmxOutput::start(&self.config, self.frame.clone()) {
            Ok(o) => self.output = Some(o),
            Err(e) => tracing::error!("DMX output unavailable: {e}"),
        }
    }

    pub fn is_transmitting(&self) -> bool {
        self.output.is_some()
    }

    pub fn handle(&mut self, now: ShowTime, action: &DmxAction, lighting: &LightingConfig) {
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
        let scenes: Vec<_> = self.console.palette_scenes().map(|(c, (a, p))| (*c, a.clone(), p.clone())).collect();
        for (cue, attributes, palettes) in scenes {
            let mut values = resolve_palettes(&palettes, lighting, &self.patch);
            values.extend(attributes);
            self.playback.apply_scene(now, cue, &[], &values, ShowTime::from_millis(300), &self.patch);
        }
    }

    /// Renders home values, playback and the console layers into the
    /// universes and publishes the frame to the sender.
    pub fn tick(&mut self, now: ShowTime, lighting: &LightingConfig) {
        let mut layer: BTreeMap<crate::playback::Slot, SlotValue> = BTreeMap::new();
        for r in self.patch.fixtures().flat_map(|(f, _)| self.patch.attributes_of(f.id)).map(|(_, r)| r) {
            layer.insert(crate::playback::Slot { universe: r.universe, channel: r.coarse }, SlotValue { value: r.default, fine: r.fine, kind: r.kind });
        }
        for (slot, value, fine, kind) in self.playback.values(now) {
            layer.insert(slot, SlotValue { value, fine, kind });
        }
        self.console.apply(&mut layer, now, lighting, &self.patch);
        self.last.clear();
        for (slot, v) in &layer {
            self.last.insert(*slot, v.value);
            let Some(u) = self.universes.get_mut(&slot.universe) else { continue };
            match v.fine {
                Some(fine) => {
                    let v16 = (v.value.clamp(0.0, 1.0) * 65_535.0).round() as u16;
                    u.set(slot.channel, (v16 >> 8) as u8);
                    u.set(fine, (v16 & 0xFF) as u8);
                }
                None => u.set(slot.channel, (v.value.clamp(0.0, 1.0) * 255.0).round() as u8),
            }
        }
        let mut f = self.frame.lock();
        f.clone_from(&self.universes);
    }

    /// Programmer: set `attribute` on the selected fixtures that have it.
    pub fn programmer_set(&mut self, attribute: &str, value: f32) {
        self.console.programmer.set_selected(attribute, value, &self.patch);
    }

    /// Programmer: release `attribute` on the selection (back to playback).
    pub fn programmer_release(&mut self, attribute: &str) {
        for f in self.console.programmer.selection.clone() {
            self.console.programmer.release(f, attribute);
        }
    }

    /// Programmer: apply a palette to the selection. Returns values set.
    pub fn programmer_apply_palette(&mut self, palette: &livestage_core::lighting::Palette) -> usize {
        self.console.programmer.apply_palette(palette, &self.patch)
    }

    /// Programmer values of one category on the selection, as palette data.
    pub fn programmer_palette_values(&self, kind: livestage_core::lighting::PaletteKind) -> Vec<livestage_core::lighting::LightValue> {
        self.console.programmer.palette_values(kind, &self.patch)
    }

    /// Output value (0..=1) of a fixture attribute after the last tick.
    pub fn output(&self, fixture: FixtureId, attribute: &str) -> Option<f32> {
        let r = self.patch.resolve(fixture, attribute)?;
        self.last.get(&crate::playback::Slot { universe: r.universe, channel: r.coarse }).copied()
    }

    /// Approximate emitted colour (0..=1 RGB, intensity applied) for the stage view.
    pub fn fixture_color(&self, fixture: FixtureId) -> Option<[f32; 3]> {
        fixture_color(&self.patch, fixture, |a| self.output(fixture, a))
    }

    /// Every slot value written by the last tick (for UI snapshots).
    pub fn last_values(&self) -> &BTreeMap<crate::playback::Slot, f32> {
        &self.last
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
/// given its attribute output values.
pub fn fixture_color(patch: &Patch, fixture: FixtureId, get: impl Fn(&str) -> Option<f32>) -> Option<[f32; 3]> {
    let attrs = patch.attributes_of(fixture);
    if attrs.is_empty() {
        return None;
    }
    let dimmer = if attrs.iter().any(|(_, r)| r.kind.is_intensity()) { attrs.iter().find(|(_, r)| r.kind.is_intensity()).and_then(|(n, _)| get(n)).unwrap_or(0.0) } else { 1.0 };
    let has_rgb = get("red").is_some();
    let mut c = if has_rgb {
        let w = get("white").unwrap_or(0.0) + get("amber").unwrap_or(0.0) * 0.5;
        [get("red").unwrap_or(0.0) + w, get("green").unwrap_or(0.0) + w * 0.8, get("blue").unwrap_or(0.0) + w * 0.6]
    } else {
        [1.0, 0.95, 0.85]
    };
    for v in &mut c {
        *v = (*v * dimmer).clamp(0.0, 1.0);
    }
    Some(c)
}
