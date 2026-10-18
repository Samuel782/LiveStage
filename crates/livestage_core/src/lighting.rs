//! Lighting console data: palettes, submasters, effects and the stage layout.
//!
//! These are show data (saved in the manifest). The live side — the
//! programmer, running effects, flash buttons — belongs to the DMX engine.
//!
//! * **Palettes** hold attribute values by category (colour, position,
//!   beam, intensity), either for any fixture that has the attribute or
//!   for one fixture (positions differ per fixture). DMX cues reference
//!   palettes by id and resolve them when they run, so editing a palette
//!   updates every cue that uses it, including looks on stage right now.
//! * **Submasters** are busking faders: additive looks (blinders), software
//!   strobes and inhibitive group masters; a grand master scales them all.
//! * **Effects** are generators layered on top of playback: pan / tilt
//!   sines and circles, colour chases and dimmer chases, with phase spread
//!   across fixtures.

use crate::id::{EffectId, FixtureId, PaletteId, SubmasterId};
use crate::state::Rgb;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteKind {
    Intensity,
    Color,
    Position,
    Beam,
}

impl PaletteKind {
    pub const ALL: [PaletteKind; 4] = [PaletteKind::Intensity, PaletteKind::Color, PaletteKind::Position, PaletteKind::Beam];
    pub fn label(self) -> &'static str {
        match self {
            PaletteKind::Intensity => "Intensity",
            PaletteKind::Color => "Colour",
            PaletteKind::Position => "Position",
            PaletteKind::Beam => "Beam / Gobo",
        }
    }
}

/// One attribute value in a palette or additive submaster.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LightValue {
    /// None = every fixture that has the attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture: Option<FixtureId>,
    /// Attribute name (`"red"`, `"pan"`, `"gobo"`…), case-insensitive.
    pub attribute: String,
    /// Normalised 0..=1.
    pub value: f32,
}

impl LightValue {
    pub fn any(attribute: &str, value: f32) -> Self {
        Self { fixture: None, attribute: attribute.into(), value }
    }
    pub fn of(fixture: FixtureId, attribute: &str, value: f32) -> Self {
        Self { fixture: Some(fixture), attribute: attribute.into(), value }
    }
}

/// Resolves values for `fixture`: fixture-specific entries override
/// "any fixture" ones. Attribute names are lower-cased.
pub fn values_for(values: &[LightValue], fixture: FixtureId) -> BTreeMap<String, f32> {
    let mut out = BTreeMap::new();
    for v in values.iter().filter(|v| v.fixture.is_none()) {
        out.insert(v.attribute.to_ascii_lowercase(), v.value);
    }
    for v in values.iter().filter(|v| v.fixture == Some(fixture)) {
        out.insert(v.attribute.to_ascii_lowercase(), v.value);
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Palette {
    pub id: PaletteId,
    pub name: String,
    pub kind: PaletteKind,
    /// Swatch shown on the palette button.
    #[serde(default)]
    pub swatch: Option<Rgb>,
    pub values: Vec<LightValue>,
}

impl Palette {
    pub fn new(name: impl Into<String>, kind: PaletteKind, values: Vec<LightValue>) -> Self {
        Self { id: PaletteId::new(), name: name.into(), kind, swatch: None, values }
    }
    pub fn values_for(&self, fixture: FixtureId) -> BTreeMap<String, f32> {
        values_for(&self.values, fixture)
    }
}

/// A cue's use of a palette on some fixtures.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaletteRef {
    pub palette: PaletteId,
    pub fixtures: Vec<FixtureId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubmasterKind {
    /// Adds a look scaled by the fader: intensities highest-takes-precedence,
    /// other attributes crossfade towards the look (blinders, ACL looks).
    Additive { values: Vec<LightValue> },
    /// Software strobe on the fixtures' intensity: the fader sets the rate.
    Strobe { min_hz: f32, max_hz: f32 },
    /// Scales the fixtures' intensity (group master).
    Inhibit,
}

impl SubmasterKind {
    pub fn label(&self) -> &'static str {
        match self {
            SubmasterKind::Additive { .. } => "Look",
            SubmasterKind::Strobe { .. } => "Strobe",
            SubmasterKind::Inhibit => "Inhibit",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Submaster {
    pub id: SubmasterId,
    pub name: String,
    pub kind: SubmasterKind,
    /// Fixtures it acts on (empty = every patched fixture).
    #[serde(default)]
    pub fixtures: Vec<FixtureId>,
    /// Fader level 0..=1.
    #[serde(default)]
    pub level: f32,
    #[serde(default)]
    pub color: Option<Rgb>,
}

impl Submaster {
    pub fn new(name: impl Into<String>, kind: SubmasterKind, fixtures: Vec<FixtureId>) -> Self {
        let level = if matches!(kind, SubmasterKind::Inhibit) { 1.0 } else { 0.0 };
        Self { id: SubmasterId::new(), name: name.into(), kind, fixtures, level, color: None }
    }
    pub fn applies_to(&self, fixture: FixtureId) -> bool {
        self.fixtures.is_empty() || self.fixtures.contains(&fixture)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EffectKind {
    /// Sine wave on one attribute around its current value.
    Sine { attribute: String },
    /// Pan / tilt circle around the current position.
    Circle,
    /// Steps through colours (RGB fixtures).
    ColorChase { colors: Vec<Rgb> },
    /// Intensity pulses (a chase when spread across fixtures).
    DimmerChase,
}

impl EffectKind {
    pub fn label(&self) -> &'static str {
        match self {
            EffectKind::Sine { .. } => "Sine",
            EffectKind::Circle => "Circle",
            EffectKind::ColorChase { .. } => "Colour chase",
            EffectKind::DimmerChase => "Dimmer chase",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub id: EffectId,
    pub name: String,
    pub kind: EffectKind,
    pub fixtures: Vec<FixtureId>,
    /// Cycles per second.
    pub rate_hz: f32,
    /// Amplitude (0..=1 of the attribute range; colour chases: mix amount).
    pub size: f32,
    /// Phase spread across the fixtures, 0 (in unison) ..= 1 (one full cycle).
    pub spread: f32,
}

impl Effect {
    pub fn new(name: impl Into<String>, kind: EffectKind, fixtures: Vec<FixtureId>) -> Self {
        Self { id: EffectId::new(), name: name.into(), kind, fixtures, rate_hz: 0.25, size: 0.3, spread: 0.0 }
    }

    /// Phase (in cycles) of fixture number `index` of `count` after `t` seconds.
    pub fn phase(&self, t: f64, index: usize, count: usize) -> f64 {
        let offset = if count > 1 { self.spread as f64 * index as f64 / count as f64 } else { 0.0 };
        (t * self.rate_hz as f64 + offset).rem_euclid(1.0)
    }
}

/// Everything the lighting console saves with the show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightingConfig {
    pub palettes: Vec<Palette>,
    pub submasters: Vec<Submaster>,
    pub effects: Vec<Effect>,
    pub grand_master: f32,
    /// Fixture positions on the stage plot, normalised (0,0 = upstage left).
    pub stage: BTreeMap<FixtureId, [f32; 2]>,
}

impl Default for LightingConfig {
    fn default() -> Self {
        Self { palettes: vec![], submasters: vec![], effects: vec![], grand_master: 1.0, stage: BTreeMap::new() }
    }
}

impl LightingConfig {
    pub fn palette(&self, id: PaletteId) -> Option<&Palette> {
        self.palettes.iter().find(|p| p.id == id)
    }
    pub fn effect(&self, id: EffectId) -> Option<&Effect> {
        self.effects.iter().find(|e| e.id == id)
    }
    /// Live fader positions (kept when an edit is undone mid-show).
    pub fn levels(&self) -> (f32, Vec<(SubmasterId, f32)>) {
        (self.grand_master, self.submasters.iter().map(|s| (s.id, s.level)).collect())
    }
    pub fn set_levels(&mut self, (gm, subs): &(f32, Vec<(SubmasterId, f32)>)) {
        self.grand_master = *gm;
        for s in &mut self.submasters {
            if let Some((_, l)) = subs.iter().find(|(id, _)| *id == s.id) {
                s.level = *l;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_values_override_global_ones() {
        let a = FixtureId::new();
        let b = FixtureId::new();
        let p = Palette::new(
            "Warm",
            PaletteKind::Color,
            vec![LightValue::any("Red", 1.0), LightValue::any("green", 0.5), LightValue::of(b, "green", 0.2)],
        );
        assert_eq!(p.values_for(a).get("green"), Some(&0.5));
        assert_eq!(p.values_for(b).get("green"), Some(&0.2));
        assert_eq!(p.values_for(b).get("red"), Some(&1.0), "names are case-insensitive");
    }

    #[test]
    fn effect_phase_spreads_across_fixtures() {
        let mut e = Effect::new("Wave", EffectKind::DimmerChase, vec![]);
        e.rate_hz = 0.5;
        assert!((e.phase(1.0, 0, 4) - 0.5).abs() < 1e-9);
        e.spread = 1.0;
        assert!((e.phase(0.0, 1, 4) - 0.25).abs() < 1e-9);
        assert!((e.phase(0.0, 3, 4) - 0.75).abs() < 1e-9);
        assert!(e.phase(10.3, 2, 4) < 1.0);
    }

    #[test]
    fn config_roundtrip_and_levels() {
        let fx = FixtureId::new();
        let mut c = LightingConfig::default();
        c.palettes.push(Palette::new("Red", PaletteKind::Color, vec![LightValue::any("red", 1.0)]));
        c.submasters.push(Submaster::new("Blinders", SubmasterKind::Additive { values: vec![LightValue::any("intensity", 1.0)] }, vec![fx]));
        c.submasters.push(Submaster::new("Strobe", SubmasterKind::Strobe { min_hz: 1.0, max_hz: 15.0 }, vec![]));
        c.effects.push(Effect::new("Circle", EffectKind::Circle, vec![fx]));
        c.stage.insert(fx, [0.5, 0.2]);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<LightingConfig>(&json).unwrap(), c);
        assert_eq!(serde_json::from_str::<LightingConfig>("{}").unwrap().grand_master, 1.0);
        let levels = (0.4, vec![(c.submasters[0].id, 0.9)]);
        c.set_levels(&levels);
        assert_eq!(c.levels().0, 0.4);
        assert_eq!(c.submasters[0].level, 0.9);
    }
}
