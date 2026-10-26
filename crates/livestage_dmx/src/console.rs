//! The live side of the lighting console: the programmer, running effects,
//! submaster flash buttons and the layered render that turns playback into
//! output.
//!
//! ## Render order (every DMX render, see [`crate::DmxEngine::tick`])
//!
//! 1. **Home** values for every patched attribute (pan centred, shutter open…)
//! 2. **Playback** — DMX cues (LTP crossfades)
//! 3. **Programmer** — live edits, latest takes precedence
//! 4. **Effects** — pan/tilt sines and circles, colour and dimmer chases
//! 5. **Submaster looks** — intensities HTP, other attributes crossfaded
//! 6. **Strobe** submasters
//! 7. **Inhibit** submasters, then **grand master** and **blackout**
//!
//! Intensity scaling (steps 6–7) acts on each fixture's *dimmer*: its
//! intensity channel, or — for fixtures without one, like 3-channel RGB
//! pars — its colour emitters.

use crate::fixture::{AttributeKind, Patch};
use livestage_core::action::FixtureAttributeValue;
use livestage_core::id::{CueId, EffectId, FixtureId, PaletteId, SubmasterId};
use livestage_core::lighting::{EffectKind, LightingConfig, Palette, PaletteKind, PaletteRef, Submaster, SubmasterKind};
use livestage_core::time::ShowTime;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::f64::consts::TAU;

/// Live values typed or dialled in by the operator.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Programmer {
    /// Selected fixtures, in selection order.
    pub selection: Vec<FixtureId>,
    values: BTreeMap<(FixtureId, String), f32>,
    /// Attributes whose value came from a palette (recorded as a reference).
    from_palette: BTreeMap<(FixtureId, String), PaletteId>,
}

fn key(fixture: FixtureId, attribute: &str) -> (FixtureId, String) {
    (fixture, attribute.to_ascii_lowercase())
}

impl Programmer {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn get(&self, fixture: FixtureId, attribute: &str) -> Option<f32> {
        self.values.get(&key(fixture, attribute)).copied()
    }
    pub fn values(&self) -> impl Iterator<Item = (FixtureId, &str, f32)> + '_ {
        self.values.iter().map(|((f, a), v)| (*f, a.as_str(), *v))
    }

    pub fn set(&mut self, fixture: FixtureId, attribute: &str, value: f32) {
        let k = key(fixture, attribute);
        self.from_palette.remove(&k);
        self.values.insert(k, value.clamp(0.0, 1.0));
    }

    /// Sets `attribute` on every selected fixture that has it.
    pub fn set_selected(&mut self, attribute: &str, value: f32, patch: &Patch) {
        for f in self.selection.clone() {
            if patch.has_attribute(f, attribute) {
                self.set(f, attribute, value);
            }
        }
    }

    /// Removes one attribute (it falls back to playback).
    pub fn release(&mut self, fixture: FixtureId, attribute: &str) {
        let k = key(fixture, attribute);
        self.values.remove(&k);
        self.from_palette.remove(&k);
    }

    /// Clears values (selection is kept).
    pub fn clear(&mut self) {
        self.values.clear();
        self.from_palette.clear();
    }

    /// Applies a palette to the selected fixtures (by reference).
    pub fn apply_palette(&mut self, palette: &Palette, patch: &Patch) -> usize {
        let mut n = 0;
        for f in self.selection.clone() {
            for (attr, v) in palette.values_for(f) {
                if patch.has_attribute(f, &attr) {
                    let k = key(f, &attr);
                    self.values.insert(k.clone(), v);
                    self.from_palette.insert(k, palette.id);
                    n += 1;
                }
            }
        }
        n
    }

    /// The programmer as cue data: plain values plus palette references
    /// (so cues follow later palette edits).
    pub fn record(&self) -> (Vec<FixtureAttributeValue>, Vec<PaletteRef>) {
        let mut attrs = Vec::new();
        let mut refs: BTreeMap<PaletteId, Vec<FixtureId>> = BTreeMap::new();
        for ((f, a), v) in &self.values {
            match self.from_palette.get(&(*f, a.clone())) {
                Some(p) => {
                    let list = refs.entry(*p).or_default();
                    if !list.contains(f) {
                        list.push(*f);
                    }
                }
                None => attrs.push(FixtureAttributeValue { fixture: *f, attribute: a.clone(), value: *v }),
            }
        }
        (attrs, refs.into_iter().map(|(palette, fixtures)| PaletteRef { palette, fixtures }).collect())
    }

    /// Programmer values of one category on the selection, as palette data.
    /// Positions are stored per fixture; other categories once for every
    /// fixture that has the attribute (taken from the first selected one).
    pub fn palette_values(&self, kind: PaletteKind, patch: &Patch) -> Vec<livestage_core::lighting::LightValue> {
        use livestage_core::lighting::LightValue;
        let category = |f: FixtureId, a: &str| patch.resolve(f, a).and_then(|r| r.kind.category());
        let mut out: Vec<LightValue> = Vec::new();
        for f in &self.selection {
            for ((vf, a), v) in self.values.range((*f, String::new())..) {
                if vf != f {
                    break;
                }
                if category(*f, a) != Some(kind) {
                    continue;
                }
                if kind == PaletteKind::Position {
                    out.push(LightValue::of(*f, a, *v));
                } else if !out.iter().any(|o| o.attribute == *a) {
                    out.push(LightValue::any(a, *v));
                }
            }
        }
        out
    }
}

/// A running effect: started by the console and / or by DMX cues.
#[derive(Debug, Clone, PartialEq)]
struct FxRun {
    started: ShowTime,
    console: bool,
    cues: Vec<CueId>,
}

/// One slot's value while layers are applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotValue {
    pub value: f32,
    pub fine: Option<u16>,
    pub kind: AttributeKind,
}

pub type Slot = crate::playback::Slot;

/// Dense per-universe slot values for one render. Planes are created the
/// first time a universe is touched and kept, so a render after the first
/// allocates nothing; [`Layer::reset_from`] copies a prebuilt base layer
/// (home values) in place.
#[derive(Debug, Clone, Default)]
pub struct Layer {
    /// Sorted by universe; index `channel - 1`.
    planes: Vec<(u16, Box<[Option<SlotValue>; 512]>)>,
}

impl Layer {
    fn plane_index(&self, universe: u16) -> Result<usize, usize> {
        self.planes.binary_search_by_key(&universe, |(u, _)| *u)
    }

    fn index(channel: u16) -> Option<usize> {
        (1..=512).contains(&channel).then(|| channel as usize - 1)
    }

    pub fn get(&self, slot: Slot) -> Option<&SlotValue> {
        let i = Self::index(slot.channel)?;
        self.planes.get(self.plane_index(slot.universe).ok()?)?.1[i].as_ref()
    }

    pub fn get_mut(&mut self, slot: Slot) -> Option<&mut SlotValue> {
        let i = Self::index(slot.channel)?;
        let p = self.plane_index(slot.universe).ok()?;
        self.planes[p].1[i].as_mut()
    }

    /// The value at `slot`, inserting `default` if empty. `None` for an
    /// out-of-range channel.
    pub fn entry(&mut self, slot: Slot, default: SlotValue) -> Option<&mut SlotValue> {
        let i = Self::index(slot.channel)?;
        let p = match self.plane_index(slot.universe) {
            Ok(p) => p,
            Err(p) => {
                self.planes.insert(p, (slot.universe, Box::new([None; 512])));
                p
            }
        };
        Some(self.planes[p].1[i].get_or_insert(default))
    }

    pub fn insert(&mut self, slot: Slot, value: SlotValue) {
        if let Some(e) = self.entry(slot, value) {
            *e = value;
        }
    }

    /// Makes this layer equal to `base` without reallocating planes it has.
    pub fn reset_from(&mut self, base: &Layer) {
        for (u, plane) in &mut self.planes {
            match base.plane_index(*u) {
                Ok(b) => **plane = *base.planes[b].1,
                Err(_) => plane.fill(None),
            }
        }
        for (u, plane) in &base.planes {
            if let Err(p) = self.plane_index(*u) {
                self.planes.insert(p, (*u, plane.clone()));
            }
        }
    }

    /// Each universe plane (index `channel - 1`), in universe order.
    pub fn planes(&self) -> impl Iterator<Item = (u16, &[Option<SlotValue>; 512])> + '_ {
        self.planes.iter().map(|(u, p)| (*u, &**p))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Slot, &mut SlotValue)> + '_ {
        self.planes.iter_mut().flat_map(|(u, plane)| {
            let u = *u;
            plane.iter_mut().enumerate().filter_map(move |(i, v)| v.as_mut().map(|v| (Slot { universe: u, channel: i as u16 + 1 }, v)))
        })
    }
}

/// Console state owned by the DMX engine.
#[derive(Debug, Default)]
pub struct Console {
    pub programmer: Programmer,
    fx: HashMap<EffectId, FxRun>,
    /// Submasters whose flash button is held.
    pub flashes: HashSet<SubmasterId>,
    /// Desk blackout (DBO): intensities to zero, everything else holds.
    pub blackout: bool,
    /// Palette-referencing scenes on stage: (explicit attributes, palette refs).
    scenes: HashMap<CueId, (Vec<FixtureAttributeValue>, Vec<PaletteRef>)>,
}

impl Console {
    // ------------------------------------------------------------ effects --

    pub fn set_effect(&mut self, id: EffectId, running: bool, now: ShowTime) {
        if running {
            self.fx.entry(id).or_insert(FxRun { started: now, console: false, cues: vec![] }).console = true;
        } else if let Some(r) = self.fx.get_mut(&id) {
            r.console = false;
            if r.cues.is_empty() {
                self.fx.remove(&id);
            }
        }
    }

    pub fn effect_running(&self, id: EffectId) -> bool {
        self.fx.contains_key(&id)
    }

    /// Effects running from the console (recorded into cues).
    pub fn console_effects(&self) -> Vec<EffectId> {
        self.fx.iter().filter(|(_, r)| r.console).map(|(id, _)| *id).collect()
    }

    pub fn stop_effects(&mut self) {
        self.fx.clear();
    }

    pub(crate) fn cue_started(&mut self, cue: CueId, effects: &[EffectId], attributes: &[FixtureAttributeValue], palettes: &[PaletteRef], now: ShowTime) {
        for e in effects {
            let r = self.fx.entry(*e).or_insert(FxRun { started: now, console: false, cues: vec![] });
            if !r.cues.contains(&cue) {
                r.cues.push(cue);
            }
        }
        if palettes.is_empty() {
            self.scenes.remove(&cue);
        } else {
            self.scenes.insert(cue, (attributes.to_vec(), palettes.to_vec()));
        }
    }

    pub(crate) fn cue_released(&mut self, cue: CueId) {
        self.scenes.remove(&cue);
        self.fx.retain(|_, r| {
            r.cues.retain(|c| *c != cue);
            r.console || !r.cues.is_empty()
        });
    }

    /// Scenes to re-resolve after a palette edit.
    pub(crate) fn palette_scenes(&self) -> impl Iterator<Item = (&CueId, &(Vec<FixtureAttributeValue>, Vec<PaletteRef>))> {
        self.scenes.iter()
    }

    // ------------------------------------------------------------- render --

    /// Applies programmer, effects, submasters and masters on top of `layer`
    /// (home + playback values).
    pub fn apply(&self, layer: &mut Layer, now: ShowTime, lighting: &LightingConfig, patch: &Patch) {
        let resolve = |f: FixtureId, a: &str| patch.resolve(f, a).map(|r| (r.slot(), r));
        let set = |layer: &mut Layer, f: FixtureId, a: &str, op: &dyn Fn(f32) -> f32| {
            if let Some((slot, r)) = resolve(f, a) {
                if let Some(e) = layer.entry(slot, SlotValue { value: r.default, fine: r.fine, kind: r.kind }) {
                    e.value = op(e.value).clamp(0.0, 1.0);
                }
            }
        };

        // Programmer (LTP).
        for ((f, a), v) in &self.programmer.values {
            set(layer, *f, a, &|_| *v);
        }

        // Effects.
        // Stable order (no allocation when nothing runs).
        let mut running: Vec<(EffectId, &FxRun)> = self.fx.iter().map(|(id, r)| (*id, r)).collect();
        running.sort_by_key(|(id, _)| *id);
        for (id, run) in &running {
            let Some(fx) = lighting.effect(*id) else { continue };
            let t = (now - run.started).as_secs_f64();
            let patched = || fx.fixtures.iter().copied().filter(|f| patch.fixture(*f).is_some());
            let n = patched().count();
            let size = fx.size.clamp(0.0, 1.0);
            for (i, f) in patched().enumerate() {
                let phase = fx.phase(t, i, n);
                let s = (TAU * phase).sin() as f32;
                let c = (TAU * phase).cos() as f32;
                match &fx.kind {
                    EffectKind::Sine { attribute } => set(layer, f, attribute, &|v| v + size * 0.5 * s),
                    EffectKind::Circle => {
                        set(layer, f, "pan", &|v| v + size * 0.5 * c);
                        set(layer, f, "tilt", &|v| v + size * 0.5 * s);
                    }
                    EffectKind::ColorChase { colors } if !colors.is_empty() => {
                        let col = colors[((phase * colors.len() as f64) as usize).min(colors.len() - 1)];
                        for (a, target) in [("red", col.0), ("green", col.1), ("blue", col.2)] {
                            let target = target as f32 / 255.0;
                            set(layer, f, a, &|v| v + (target - v) * size);
                        }
                        if patch.has_attribute(f, "white") {
                            set(layer, f, "white", &|v| v * (1.0 - size));
                        }
                    }
                    EffectKind::ColorChase { .. } => {}
                    EffectKind::DimmerChase => {
                        // Pulses between (1 - size) and full of the current level…
                        // or, on a dark fixture, between 0 and `size`.
                        let wave = 0.5 + 0.5 * s;
                        for slot in patch.dimmer_slots(f) {
                            if let Some(e) = layer.get_mut(*slot) {
                                e.value = if e.value > 0.0 { e.value * (1.0 - size + size * wave) } else { size * wave };
                            }
                        }
                    }
                }
            }
        }

        // Submasters.
        let secs = now.as_secs_f64();
        let sub_fixtures = |sub| submaster_fixtures(sub, patch);
        for sub in &lighting.submasters {
            let level = if self.flashes.contains(&sub.id) { 1.0 } else { sub.level.clamp(0.0, 1.0) };
            match &sub.kind {
                SubmasterKind::Additive { values } if level > 0.0 => {
                    // Looks without per-fixture values resolve once, not per fixture.
                    let shared = values.iter().all(|v| v.fixture.is_none());
                    let mut cached = None;
                    for f in sub_fixtures(sub) {
                        let owned;
                        let look = if shared {
                            cached.get_or_insert_with(|| livestage_core::lighting::values_for(values, f))
                        } else {
                            owned = livestage_core::lighting::values_for(values, f);
                            &owned
                        };
                        for (a, target) in look {
                            let Some((slot, r)) = resolve(f, a) else { continue };
                            if let Some(e) = layer.entry(slot, SlotValue { value: r.default, fine: r.fine, kind: r.kind }) {
                                let v = e.value;
                                // Intensities HTP, everything else crossfades towards the look.
                                e.value = if r.kind.is_intensity() { v.max(target * level) } else { v + (target - v) * level }.clamp(0.0, 1.0);
                            }
                        }
                    }
                }
                SubmasterKind::Strobe { min_hz, max_hz } if level > 0.0 => {
                    let hz = (*min_hz + (*max_hz - *min_hz) * level).max(0.1) as f64;
                    // 30 % duty: a bright, short flash.
                    let on = (secs * hz).rem_euclid(1.0) < 0.3;
                    for f in sub_fixtures(sub) {
                        for slot in patch.dimmer_slots(f) {
                            if let Some(e) = layer.get_mut(*slot) {
                                e.value = if on { 1.0 } else { 0.0 };
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Inhibits scale after every look and strobe (they multiply, so order among them is free).
        for sub in lighting.submasters.iter().filter(|s| matches!(s.kind, SubmasterKind::Inhibit)) {
            let level = if self.flashes.contains(&sub.id) { 1.0 } else { sub.level.clamp(0.0, 1.0) };
            if level >= 1.0 {
                continue;
            }
            for f in sub_fixtures(sub) {
                for slot in patch.dimmer_slots(f) {
                    if let Some(e) = layer.get_mut(*slot) {
                        e.value *= level;
                    }
                }
            }
        }

        // Grand master and blackout: every dimmer, including raw channels.
        let gm = if self.blackout { 0.0 } else { lighting.grand_master.clamp(0.0, 1.0) };
        if gm < 1.0 {
            for (_, e) in layer.iter_mut() {
                if e.kind.is_intensity() {
                    e.value *= gm;
                }
            }
            // Emitters acting as dimmers (deduplicated in the patch); skip
            // any a raw channel already turned into an intensity above.
            for slot in patch.virtual_dimmers() {
                if let Some(e) = layer.get_mut(*slot).filter(|e| !e.kind.is_intensity()) {
                    e.value *= gm;
                }
            }
        }
    }
}

/// A submaster's fixtures: its own list (patched ones only), or every
/// patched fixture when the list is empty. Avoids `Submaster::applies_to`
/// per patched fixture, which is O(n²) on big rigs.
fn submaster_fixtures<'a>(sub: &'a Submaster, patch: &'a Patch) -> impl Iterator<Item = FixtureId> + 'a {
    let all = if sub.fixtures.is_empty() { usize::MAX } else { 0 };
    patch.fixtures().take(all).map(|(pf, _)| pf.id).chain(sub.fixtures.iter().copied().filter(|f| patch.fixture(*f).is_some()))
}

/// Expands palette references into attribute values (only attributes the
/// fixtures actually have).
pub fn resolve_palettes(refs: &[PaletteRef], lighting: &LightingConfig, patch: &Patch) -> Vec<FixtureAttributeValue> {
    let mut out = Vec::new();
    for r in refs {
        let Some(p) = lighting.palette(r.palette) else {
            tracing::warn!(palette = ?r.palette, "cue references a deleted palette");
            continue;
        };
        for f in &r.fixtures {
            for (attribute, value) in p.values_for(*f) {
                if patch.has_attribute(*f, &attribute) {
                    out.push(FixtureAttributeValue { fixture: *f, attribute, value });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DmxEngine;
    use livestage_core::action::DmxAction;
    use livestage_core::dmx_config::{DmxConfig, PatchedFixture};
    use livestage_core::lighting::{Effect, LightValue, Palette};
    use livestage_core::state::Rgb;

    struct Rig {
        dmx: DmxEngine,
        lighting: LightingConfig,
        head: FixtureId,
        rgb: FixtureId,
        rgbw: FixtureId,
    }

    fn rig() -> Rig {
        let fx = |name: &str, def: &str, mode: &str, address: u16| PatchedFixture {
            id: FixtureId::new(),
            name: name.into(),
            definition: def.into(),
            mode: mode.into(),
            universe: 0,
            address,
        };
        let head = fx("Spot", "generic/moving-head", "12ch", 1);
        let rgb = fx("Par RGB", "generic/rgb-par", "3ch", 20);
        let rgbw = fx("Par RGBW", "generic/rgbw-par", "5ch", 30);
        let ids = (head.id, rgb.id, rgbw.id);
        let cfg = DmxConfig { patch: vec![head, rgb, rgbw], ..DmxConfig::default() };
        Rig { dmx: DmxEngine::new(cfg), lighting: LightingConfig::default(), head: ids.0, rgb: ids.1, rgbw: ids.2 }
    }

    impl Rig {
        fn tick(&mut self, ms: u64) {
            self.dmx.tick(ShowTime::from_millis(ms), &self.lighting);
        }
        fn out(&self, f: FixtureId, a: &str) -> f32 {
            self.dmx.output(f, a).unwrap()
        }
        fn ch(&self, channel: u16) -> u8 {
            self.dmx.universe(0).unwrap().get(channel)
        }
        fn scene(&mut self, cue: CueId, attrs: Vec<FixtureAttributeValue>, palettes: Vec<PaletteRef>, effects: Vec<EffectId>) {
            let action = DmxAction::ApplyScene { cue, channels: vec![], attributes: attrs, palettes, effects, fade: ShowTime::ZERO };
            self.dmx.handle(ShowTime::ZERO, &action, &self.lighting);
        }
    }

    fn av(f: FixtureId, a: &str, v: f32) -> FixtureAttributeValue {
        FixtureAttributeValue { fixture: f, attribute: a.into(), value: v }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn home_values_then_playback_then_programmer() {
        let mut r = rig();
        r.tick(0);
        // Homes: pan centred (16-bit), shutter open, dark.
        assert_eq!((r.ch(1), r.ch(2)), (128, 0));
        assert_eq!(r.ch(7), 255, "shutter open");
        assert_eq!(r.ch(5), 0);
        r.scene(CueId::new(), vec![av(r.head, "intensity", 0.5), av(r.head, "pan", 0.25)], vec![], vec![]);
        r.tick(10);
        assert!(close(r.out(r.head, "intensity"), 0.5));
        // The programmer overrides playback (LTP) on the selected fixtures that have the attribute.
        r.dmx.console.programmer.selection = vec![r.head, r.rgb];
        let patch = r.dmx.patch().clone();
        r.dmx.console.programmer.set_selected("pan", 0.75, &patch);
        r.tick(20);
        assert!(close(r.out(r.head, "pan"), 0.75));
        assert!(close(r.out(r.head, "intensity"), 0.5), "other attributes still from playback");
        assert_eq!(r.dmx.console.programmer.len(), 1, "the RGB par has no pan");
        r.dmx.console.programmer.clear();
        r.tick(30);
        assert!(close(r.out(r.head, "pan"), 0.25), "cleared programmer releases to playback");
    }

    #[test]
    fn palettes_are_referenced_and_follow_edits() {
        let mut r = rig();
        let red = Palette::new("Red", PaletteKind::Color, vec![LightValue::any("red", 1.0), LightValue::any("green", 0.0), LightValue::any("blue", 0.0)]);
        let red_id = red.id;
        r.lighting.palettes.push(red);
        // Programmer: apply the palette, add an intensity, record.
        r.dmx.console.programmer.selection = vec![r.rgb, r.rgbw];
        let patch = r.dmx.patch().clone();
        let red_palette = r.lighting.palettes[0].clone();
        assert_eq!(r.dmx.console.programmer.apply_palette(&red_palette, &patch), 6);
        r.dmx.console.programmer.set(r.rgbw, "intensity", 1.0);
        let (attrs, refs) = r.dmx.console.programmer.record();
        assert_eq!(attrs, vec![av(r.rgbw, "intensity", 1.0)], "palette values are not copied into the cue");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].palette, red_id);
        r.dmx.console.programmer.clear();

        // A cue with the reference.
        let cue = CueId::new();
        r.scene(cue, attrs, refs, vec![]);
        r.tick(0);
        assert_eq!((r.ch(20), r.ch(21)), (255, 0));
        // Edit the palette to amber: the look on stage follows.
        r.lighting.palettes[0].values = vec![LightValue::any("red", 1.0), LightValue::any("green", 0.5), LightValue::any("blue", 0.0)];
        r.dmx.palettes_changed(ShowTime::from_secs(1), &r.lighting);
        r.tick(2_000);
        assert_eq!((r.ch(20), r.ch(21)), (255, 128));
        assert_eq!(r.ch(30), 255, "explicit attribute kept");
    }

    #[test]
    fn grand_master_scales_dimmers_and_emitters() {
        let mut r = rig();
        r.scene(CueId::new(), vec![av(r.head, "intensity", 1.0), av(r.rgb, "red", 1.0), av(r.head, "pan", 1.0)], vec![], vec![]);
        r.lighting.grand_master = 0.5;
        r.tick(0);
        assert!(close(r.out(r.head, "intensity"), 0.5));
        assert!(close(r.out(r.rgb, "red"), 0.5), "RGB-only fixture: emitters are the dimmer");
        assert!(close(r.out(r.head, "pan"), 1.0), "positions are not scaled");
        r.dmx.console.blackout = true;
        r.tick(10);
        assert_eq!(r.out(r.head, "intensity"), 0.0);
        assert_eq!(r.out(r.rgb, "red"), 0.0);
    }

    #[test]
    fn submasters_add_strobe_and_inhibit() {
        let mut r = rig();
        let blinders = Submaster::new("Blinders", SubmasterKind::Additive { values: vec![LightValue::any("intensity", 1.0)] }, vec![r.rgbw]);
        let strobe = Submaster::new("Strobe", SubmasterKind::Strobe { min_hz: 2.0, max_hz: 2.0 }, vec![r.head]);
        let group = Submaster::new("Heads", SubmasterKind::Inhibit, vec![r.head]);
        let (b, s) = (blinders.id, strobe.id);
        r.lighting.submasters = vec![blinders, strobe, group];
        r.scene(CueId::new(), vec![av(r.rgbw, "intensity", 0.3), av(r.head, "intensity", 0.8)], vec![], vec![]);
        r.tick(0);
        assert!(close(r.out(r.rgbw, "intensity"), 0.3), "faders down: no effect");
        r.lighting.submasters[0].level = 0.6;
        r.tick(10);
        assert!(close(r.out(r.rgbw, "intensity"), 0.6), "HTP with the fader");
        r.lighting.submasters[0].level = 0.1;
        r.tick(20);
        assert!(close(r.out(r.rgbw, "intensity"), 0.3), "lower than playback: playback wins");
        r.dmx.console.flashes.insert(b);
        r.tick(30);
        assert!(close(r.out(r.rgbw, "intensity"), 1.0), "flash = full");
        r.dmx.console.flashes.clear();

        // Strobe at 2 Hz: on for the first 30 % of each half second.
        r.lighting.submasters[1].level = 1.0;
        r.tick(1_000);
        assert_eq!(r.out(r.head, "intensity"), 1.0);
        r.tick(1_200);
        assert_eq!(r.out(r.head, "intensity"), 0.0);
        r.lighting.submasters[1].level = 0.0;
        let _ = s;
        // Inhibit halves the heads.
        r.lighting.submasters[2].level = 0.5;
        r.tick(1_300);
        assert!(close(r.out(r.head, "intensity"), 0.4));
    }

    #[test]
    fn effects_move_and_chase_around_the_current_look() {
        let mut r = rig();
        let mut circle = Effect::new("Circle", EffectKind::Circle, vec![r.head]);
        circle.rate_hz = 1.0;
        circle.size = 0.4;
        let mut chase = Effect::new("RGB", EffectKind::ColorChase { colors: vec![Rgb(255, 0, 0), Rgb(0, 0, 255)] }, vec![r.rgb, r.rgbw]);
        chase.rate_hz = 1.0;
        chase.size = 1.0;
        chase.spread = 1.0;
        let (circle_id, chase_id) = (circle.id, chase.id);
        r.lighting.effects = vec![circle, chase];

        r.dmx.console.set_effect(circle_id, true, ShowTime::ZERO);
        r.tick(0); // phase 0: cos = 1 → pan +0.2
        assert!(close(r.out(r.head, "pan"), 0.7));
        assert!(close(r.out(r.head, "tilt"), 0.5));
        r.tick(250); // quarter turn: tilt +0.2
        assert!(close(r.out(r.head, "pan"), 0.5));
        assert!(close(r.out(r.head, "tilt"), 0.7));

        // A cue starts the colour chase; spread puts the two pars half a cycle apart.
        let cue = CueId::new();
        r.scene(cue, vec![], vec![], vec![chase_id]);
        r.tick(0);
        assert!(close(r.out(r.rgb, "red"), 1.0) && close(r.out(r.rgbw, "blue"), 1.0));
        r.tick(600);
        assert!(close(r.out(r.rgb, "blue"), 1.0) && close(r.out(r.rgbw, "red"), 1.0));
        assert_eq!(r.dmx.console.console_effects(), vec![circle_id], "cue-started effects are not console effects");
        // Releasing the cue stops its effect; the console effect keeps running.
        r.dmx.handle(ShowTime::from_secs(1), &DmxAction::Release { cue, fade: ShowTime::ZERO }, &r.lighting);
        assert!(!r.dmx.console.effect_running(chase_id));
        assert!(r.dmx.console.effect_running(circle_id));
        r.dmx.console.set_effect(circle_id, false, ShowTime::ZERO);
        r.tick(2_000);
        assert!(close(r.out(r.head, "pan"), 0.5), "back to the base value");
    }

    #[test]
    fn dimmer_chase_and_panic() {
        let mut r = rig();
        let mut wave = Effect::new("Wave", EffectKind::DimmerChase, vec![r.head]);
        wave.rate_hz = 1.0;
        wave.size = 1.0;
        let id = wave.id;
        r.lighting.effects.push(wave);
        r.dmx.console.set_effect(id, true, ShowTime::ZERO);
        r.tick(250); // sin = 1 → full
        assert!(close(r.out(r.head, "intensity"), 1.0), "lights a dark fixture");
        r.tick(750);
        assert!(close(r.out(r.head, "intensity"), 0.0));
        r.dmx.console.programmer.set(r.head, "intensity", 1.0);
        r.dmx.handle(ShowTime::from_secs(1), &DmxAction::Blackout { fade: ShowTime::ZERO }, &r.lighting);
        r.tick(1_250);
        assert_eq!(r.out(r.head, "intensity"), 0.0, "hard panic clears programmer and effects");
        assert!(r.dmx.console.programmer.is_empty());
    }

    #[test]
    fn position_palettes_record_per_fixture() {
        let mut r = rig();
        let patch = r.dmx.patch().clone();
        let p = &mut r.dmx.console.programmer;
        p.selection = vec![r.head, r.rgbw];
        p.set(r.head, "pan", 0.2);
        p.set(r.head, "tilt", 0.8);
        p.set(r.rgbw, "red", 1.0);
        let pos = p.palette_values(PaletteKind::Position, &patch);
        assert_eq!(pos, vec![LightValue::of(r.head, "pan", 0.2), LightValue::of(r.head, "tilt", 0.8)]);
        let col = p.palette_values(PaletteKind::Color, &patch);
        assert_eq!(col, vec![LightValue::any("red", 1.0)]);
    }
}
