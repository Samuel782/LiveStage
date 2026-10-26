//! Fixture definitions, the built-in library, and the patch.

use crate::playback::Slot;
use livestage_core::dmx_config::{FixtureProfile, PatchedFixture};
use livestage_core::id::FixtureId;
use livestage_core::lighting::PaletteKind;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttributeKind {
    Intensity,
    Red,
    Green,
    Blue,
    White,
    Amber,
    Uv,
    Pan,
    Tilt,
    Zoom,
    Focus,
    Gobo,
    ColorWheel,
    Shutter,
    Control,
    Generic,
}

impl AttributeKind {
    pub const ALL: [AttributeKind; 16] = [
        AttributeKind::Intensity,
        AttributeKind::Red,
        AttributeKind::Green,
        AttributeKind::Blue,
        AttributeKind::White,
        AttributeKind::Amber,
        AttributeKind::Uv,
        AttributeKind::Pan,
        AttributeKind::Tilt,
        AttributeKind::Zoom,
        AttributeKind::Focus,
        AttributeKind::Gobo,
        AttributeKind::ColorWheel,
        AttributeKind::Shutter,
        AttributeKind::Control,
        AttributeKind::Generic,
    ];

    /// Name stored in imported profiles, and the attribute name a profile
    /// gives its first attribute of this kind.
    pub fn name(self) -> &'static str {
        use AttributeKind::*;
        match self {
            Intensity => "intensity",
            Red => "red",
            Green => "green",
            Blue => "blue",
            White => "white",
            Amber => "amber",
            Uv => "uv",
            Pan => "pan",
            Tilt => "tilt",
            Zoom => "zoom",
            Focus => "focus",
            Gobo => "gobo",
            ColorWheel => "color",
            Shutter => "shutter",
            Control => "control",
            Generic => "generic",
        }
    }

    /// Inverse of [`AttributeKind::name`]; unknown names are generic.
    pub fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|k| k.name().eq_ignore_ascii_case(name)).unwrap_or(AttributeKind::Generic)
    }

    /// Attributes forced to zero by a blackout.
    pub fn is_intensity(self) -> bool {
        matches!(self, AttributeKind::Intensity)
    }

    /// Additive colour emitters: the "virtual dimmer" of fixtures that have
    /// no intensity channel.
    pub fn is_emitter(self) -> bool {
        matches!(self, AttributeKind::Red | AttributeKind::Green | AttributeKind::Blue | AttributeKind::White | AttributeKind::Amber | AttributeKind::Uv)
    }

    /// The palette category this attribute belongs to.
    pub fn category(self) -> Option<PaletteKind> {
        use AttributeKind::*;
        Some(match self {
            Intensity => PaletteKind::Intensity,
            Red | Green | Blue | White | Amber | Uv | ColorWheel => PaletteKind::Color,
            Pan | Tilt => PaletteKind::Position,
            Zoom | Focus | Gobo | Shutter => PaletteKind::Beam,
            Control | Generic => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttributeDef {
    pub name: &'static str,
    pub kind: AttributeKind,
    /// 0-based offset of the coarse byte inside the footprint.
    pub coarse: u16,
    /// 0-based offset of the fine (LSB) byte for 16-bit attributes.
    pub fine: Option<u16>,
    /// Home value, normalised.
    pub default: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FixtureMode {
    pub name: &'static str,
    pub footprint: u16,
    pub attributes: Vec<AttributeDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FixtureDefinition {
    pub key: &'static str,
    pub manufacturer: &'static str,
    pub model: &'static str,
    pub modes: Vec<FixtureMode>,
}

impl FixtureDefinition {
    pub fn mode(&self, name: &str) -> Option<&FixtureMode> {
        self.modes.iter().find(|m| m.name.eq_ignore_ascii_case(name))
    }
}

/// Interns a name of an imported profile. Definitions use `&'static str` so
/// the patch and the render path never allocate or copy names; imported
/// names are leaked once per distinct string (bounded by the fixture types
/// ever imported, a few kilobytes).
fn intern(s: &str) -> &'static str {
    use std::sync::{LazyLock, Mutex};
    static NAMES: LazyLock<Mutex<std::collections::HashSet<&'static str>>> = LazyLock::new(Default::default);
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    match names.get(s) {
        Some(n) => n,
        None => {
            let n: &'static str = Box::leak(s.to_owned().into_boxed_str());
            names.insert(n);
            n
        }
    }
}

impl FixtureDefinition {
    /// Converts an imported profile, checking that every attribute fits its
    /// mode's footprint and that no two attributes share a channel.
    pub fn from_profile(p: &FixtureProfile) -> Result<Self, String> {
        let mut modes = Vec::with_capacity(p.modes.len());
        for m in &p.modes {
            let mut used = vec![false; m.footprint as usize];
            let mut attributes = Vec::with_capacity(m.attributes.len());
            for a in &m.attributes {
                for off in std::iter::once(a.coarse).chain(a.fine) {
                    match used.get_mut(off as usize) {
                        Some(u) if !*u => *u = true,
                        Some(_) => return Err(format!("{} / {}: channel {} is used twice", p.model, m.name, off + 1)),
                        None => return Err(format!("{} / {}: `{}` is outside the {}-channel footprint", p.model, m.name, a.name, m.footprint)),
                    }
                }
                attributes.push(AttributeDef {
                    name: intern(&a.name),
                    kind: AttributeKind::from_name(&a.kind),
                    coarse: a.coarse,
                    fine: a.fine,
                    default: a.default.clamp(0.0, 1.0),
                });
            }
            modes.push(FixtureMode { name: intern(&m.name), footprint: m.footprint, attributes });
        }
        if modes.is_empty() {
            return Err(format!("{}: no usable DMX modes", p.model));
        }
        Ok(Self { key: intern(&p.key), manufacturer: intern(&p.manufacturer), model: intern(&p.model), modes })
    }
}

const fn attr(name: &'static str, kind: AttributeKind, coarse: u16, fine: Option<u16>, default: f32) -> AttributeDef {
    AttributeDef { name, kind, coarse, fine, default }
}

/// Fixture library: the built-in generic types plus the show's imported
/// GDTF / OFL profiles ([`FixtureLibrary::with_profiles`]).
#[derive(Debug, Clone)]
pub struct FixtureLibrary {
    defs: HashMap<String, FixtureDefinition>,
}

impl Default for FixtureLibrary {
    fn default() -> Self {
        Self::builtin()
    }
}

impl FixtureLibrary {
    pub fn builtin() -> Self {
        use AttributeKind::*;
        let defs = vec![
            FixtureDefinition {
                key: "generic/dimmer",
                manufacturer: "Generic",
                model: "Dimmer",
                modes: vec![
                    FixtureMode { name: "8bit", footprint: 1, attributes: vec![attr("intensity", Intensity, 0, None, 0.0)] },
                    FixtureMode { name: "16bit", footprint: 2, attributes: vec![attr("intensity", Intensity, 0, Some(1), 0.0)] },
                ],
            },
            FixtureDefinition {
                key: "generic/rgb-par",
                manufacturer: "Generic",
                model: "RGB Par",
                modes: vec![
                    FixtureMode {
                        name: "3ch",
                        footprint: 3,
                        attributes: vec![attr("red", Red, 0, None, 0.0), attr("green", Green, 1, None, 0.0), attr("blue", Blue, 2, None, 0.0)],
                    },
                    FixtureMode {
                        name: "5ch",
                        footprint: 5,
                        attributes: vec![
                            attr("intensity", Intensity, 0, None, 0.0),
                            attr("red", Red, 1, None, 0.0),
                            attr("green", Green, 2, None, 0.0),
                            attr("blue", Blue, 3, None, 0.0),
                            attr("shutter", Shutter, 4, None, 0.0),
                        ],
                    },
                ],
            },
            FixtureDefinition {
                key: "generic/rgbw-par",
                manufacturer: "Generic",
                model: "RGBW Par",
                modes: vec![FixtureMode {
                    name: "5ch",
                    footprint: 5,
                    attributes: vec![
                        attr("intensity", Intensity, 0, None, 0.0),
                        attr("red", Red, 1, None, 0.0),
                        attr("green", Green, 2, None, 0.0),
                        attr("blue", Blue, 3, None, 0.0),
                        attr("white", White, 4, None, 0.0),
                    ],
                }],
            },
            FixtureDefinition {
                key: "generic/moving-head",
                manufacturer: "Generic",
                model: "Spot Moving Head",
                modes: vec![FixtureMode {
                    name: "12ch",
                    footprint: 12,
                    attributes: vec![
                        attr("pan", Pan, 0, Some(1), 0.5),
                        attr("tilt", Tilt, 2, Some(3), 0.5),
                        attr("intensity", Intensity, 4, Some(5), 0.0),
                        attr("shutter", Shutter, 6, None, 1.0),
                        attr("color", ColorWheel, 7, None, 0.0),
                        attr("gobo", Gobo, 8, None, 0.0),
                        attr("zoom", Zoom, 9, None, 0.5),
                        attr("focus", Focus, 10, None, 0.5),
                        attr("control", Control, 11, None, 0.0),
                    ],
                }],
            },
        ];
        Self { defs: defs.into_iter().map(|d| (d.key.to_string(), d)).collect() }
    }

    /// The built-in types plus `profiles`; profiles that cannot be used are
    /// skipped and reported.
    pub fn with_profiles(profiles: &[FixtureProfile]) -> (Self, Vec<String>) {
        let mut lib = Self::builtin();
        let mut errors = Vec::new();
        for p in profiles {
            match FixtureDefinition::from_profile(p) {
                Ok(def) => lib.register(def),
                Err(e) => errors.push(e),
            }
        }
        (lib, errors)
    }

    /// Adds (or replaces) a definition.
    pub fn register(&mut self, def: FixtureDefinition) {
        self.defs.insert(def.key.to_string(), def);
    }

    pub fn get(&self, key: &str) -> Option<&FixtureDefinition> {
        self.defs.get(key)
    }
    pub fn iter(&self) -> impl Iterator<Item = &FixtureDefinition> {
        self.defs.values()
    }
}

/// A resolved DMX slot for one attribute of one fixture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedAttribute {
    pub universe: u16,
    /// 1-based coarse channel.
    pub coarse: u16,
    pub fine: Option<u16>,
    pub kind: AttributeKind,
    pub default: f32,
}

impl ResolvedAttribute {
    /// The coarse byte's slot (the key layers and playback use).
    pub fn slot(&self) -> Slot {
        Slot { universe: self.universe, channel: self.coarse }
    }
}

/// Slots read to approximate a fixture's emitted colour (stage view, tiles),
/// resolved once when the patch is built.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ColorSlots {
    pub intensity: Option<Slot>,
    pub red: Option<Slot>,
    pub green: Option<Slot>,
    pub blue: Option<Slot>,
    pub white: Option<Slot>,
    pub amber: Option<Slot>,
}

#[derive(Debug, Clone)]
struct PatchEntry {
    fixture: PatchedFixture,
    mode: FixtureMode,
    /// Every attribute, resolved once when the patch is built.
    attributes: Vec<(&'static str, ResolvedAttribute)>,
    /// The fixture's dimmer: intensity slots, or its colour emitters.
    dimmer: Vec<Slot>,
    has_intensity: bool,
    color: ColorSlots,
}

impl PatchEntry {
    fn new(fixture: PatchedFixture, mode: FixtureMode) -> Self {
        let base = fixture.address - 1;
        let attributes: Vec<_> = mode
            .attributes
            .iter()
            .map(|a| {
                let r = ResolvedAttribute {
                    universe: fixture.universe,
                    coarse: base + a.coarse + 1,
                    fine: a.fine.map(|f| base + f + 1),
                    kind: a.kind,
                    default: a.default,
                };
                (a.name, r)
            })
            .collect();
        let has_intensity = attributes.iter().any(|(_, r)| r.kind.is_intensity());
        let dimmer = attributes
            .iter()
            .filter(|(_, r)| if has_intensity { r.kind.is_intensity() } else { r.kind.is_emitter() })
            .map(|(_, r)| r.slot())
            .collect();
        let named = |n: &str| attributes.iter().find(|(a, _)| a.eq_ignore_ascii_case(n)).map(|(_, r)| r.slot());
        let color = ColorSlots {
            intensity: attributes.iter().find(|(_, r)| r.kind.is_intensity()).map(|(_, r)| r.slot()),
            red: named("red"),
            green: named("green"),
            blue: named("blue"),
            white: named("white"),
            amber: named("amber"),
        };
        Self { fixture, mode, attributes, dimmer, has_intensity, color }
    }
}

/// Hasher for UUID keys: they are already random, so folding the bytes is
/// enough, and it is far cheaper than SipHash on the render path.
#[derive(Default)]
pub(crate) struct IdHasher(u64);

impl std::hash::Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
}

pub(crate) type IdMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<IdHasher>>;

/// Validated patch: fixture → address resolution.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    /// Fixtures in patch order.
    entries: Vec<PatchEntry>,
    index: IdMap<FixtureId, usize>,
    /// Emitter slots of fixtures without an intensity channel (sorted,
    /// deduplicated): the grand master scales these as well as intensities.
    virtual_dimmers: Vec<Slot>,
}

impl Patch {
    /// Builds the patch, reporting unknown definitions, out-of-range
    /// addresses and overlapping footprints (all problems, not just the first).
    pub fn build(fixtures: &[PatchedFixture], library: &FixtureLibrary) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut entries: Vec<PatchEntry> = Vec::new();
        let mut index = IdMap::default();
        let mut occupied: HashMap<(u16, u16), &str> = HashMap::new();
        for f in fixtures {
            let Some(def) = library.get(&f.definition) else {
                errors.push(format!("{}: unknown fixture type `{}`", f.name, f.definition));
                continue;
            };
            let Some(mode) = def.mode(&f.mode) else {
                errors.push(format!("{}: unknown mode `{}` for {}", f.name, f.mode, def.model));
                continue;
            };
            let end = f.address as u32 + mode.footprint as u32 - 1;
            if f.address == 0 || end > 512 {
                errors.push(format!("{}: address {}..{} out of range 1..512", f.name, f.address, end));
                continue;
            }
            let mut clash = None;
            for ch in f.address..=end as u16 {
                if let Some(other) = occupied.insert((f.universe, ch), &f.name) {
                    clash.get_or_insert((ch, other));
                }
            }
            if let Some((ch, other)) = clash {
                errors.push(format!("{}: overlaps {other} at {}.{ch}", f.name, f.universe + 1));
            }
            let entry = PatchEntry::new(f.clone(), mode.clone());
            match index.get(&f.id) {
                Some(&i) => entries[i] = entry, // duplicate id: the last one wins, in the first one's place
                None => {
                    index.insert(f.id, entries.len());
                    entries.push(entry);
                }
            }
        }
        let mut virtual_dimmers: Vec<Slot> =
            entries.iter().filter(|e| !e.has_intensity).flat_map(|e| e.dimmer.iter().copied()).collect();
        virtual_dimmers.sort_unstable();
        virtual_dimmers.dedup();
        (Self { entries, index, virtual_dimmers }, errors)
    }

    fn entry(&self, id: FixtureId) -> Option<&PatchEntry> {
        self.index.get(&id).map(|&i| &self.entries[i])
    }

    /// Patched fixtures in patch order.
    pub fn fixtures(&self) -> impl Iterator<Item = (&PatchedFixture, &FixtureMode)> + '_ {
        self.entries.iter().map(|e| (&e.fixture, &e.mode))
    }

    pub fn fixture(&self, id: FixtureId) -> Option<(&PatchedFixture, &FixtureMode)> {
        self.entry(id).map(|e| (&e.fixture, &e.mode))
    }

    /// Every attribute of one fixture, resolved (precomputed; no allocation).
    pub fn attributes_of(&self, id: FixtureId) -> &[(&'static str, ResolvedAttribute)] {
        self.entry(id).map_or(&[], |e| &e.attributes)
    }

    /// A fixture's dimmer: its intensity channel(s), or — for fixtures
    /// without one — its colour emitters.
    pub fn dimmer_slots(&self, id: FixtureId) -> &[Slot] {
        self.entry(id).map_or(&[], |e| &e.dimmer)
    }

    /// Colour slots of a fixture; `None` for unknown fixtures or fixtures
    /// without attributes.
    pub fn color_slots(&self, id: FixtureId) -> Option<&ColorSlots> {
        self.entry(id).filter(|e| !e.attributes.is_empty()).map(|e| &e.color)
    }

    /// Emitter slots acting as dimmers (fixtures without an intensity channel).
    pub fn virtual_dimmers(&self) -> &[Slot] {
        &self.virtual_dimmers
    }

    pub fn has_attribute(&self, id: FixtureId, attribute: &str) -> bool {
        self.resolve(id, attribute).is_some()
    }

    pub fn resolve(&self, fixture: FixtureId, attribute: &str) -> Option<ResolvedAttribute> {
        let e = self.entry(fixture)?;
        e.attributes.iter().find(|(n, _)| n.eq_ignore_ascii_case(attribute)).map(|(_, r)| *r)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str, def: &str, mode: &str, addr: u16) -> PatchedFixture {
        PatchedFixture { id: FixtureId::new(), name: name.into(), definition: def.into(), mode: mode.into(), universe: 0, address: addr }
    }

    #[test]
    fn resolves_16bit_and_detects_overlap() {
        let lib = FixtureLibrary::builtin();
        let head = fx("Spot 1", "generic/moving-head", "12ch", 101);
        let par = fx("Par 1", "generic/rgb-par", "3ch", 110); // overlaps 101..112
        let (patch, errors) = Patch::build(&[head.clone(), par], &lib);
        assert_eq!(errors.len(), 1, "{errors:?}");
        let tilt = patch.resolve(head.id, "tilt").unwrap();
        assert_eq!((tilt.coarse, tilt.fine), (103, Some(104)));
    }

    #[test]
    fn imported_profiles_join_the_library_and_patch() {
        use livestage_core::dmx_config::{ProfileAttribute, ProfileMode, ProfileSource};
        let a = |name: &str, kind: &str, coarse, fine| ProfileAttribute { name: name.into(), kind: kind.into(), coarse, fine, default: 0.0 };
        let profile = FixtureProfile {
            key: "ofl/test/wash".into(),
            manufacturer: "Test".into(),
            model: "Wash".into(),
            source: ProfileSource::Ofl,
            modes: vec![ProfileMode { name: "4ch".into(), footprint: 4, attributes: vec![a("intensity", "intensity", 0, Some(1)), a("red", "red", 2, None), a("macro", "nonsense", 3, None)] }],
        };
        let mut broken = profile.clone();
        broken.key = "ofl/test/broken".into();
        broken.modes[0].attributes.push(a("blue", "blue", 4, None));
        let (lib, errors) = FixtureLibrary::with_profiles(&[profile, broken]);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("outside the 4-channel footprint"));
        let def = lib.get("ofl/test/wash").unwrap();
        assert_eq!(def.modes[0].attributes[2].kind, AttributeKind::Generic);
        assert!(lib.get("generic/dimmer").is_some(), "built-ins stay");
        let wash = fx("Wash 1", "ofl/test/wash", "4CH", 10);
        let (patch, errors) = Patch::build(std::slice::from_ref(&wash), &lib);
        assert!(errors.is_empty());
        assert_eq!(patch.resolve(wash.id, "intensity").map(|r| (r.coarse, r.fine)), Some((10, Some(11))));
        assert_eq!(patch.dimmer_slots(wash.id).len(), 1);
    }
}
