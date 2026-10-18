//! Fixture definitions, the built-in library, and the patch.

use livestage_core::dmx_config::PatchedFixture;
use livestage_core::id::FixtureId;
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
    /// Attributes forced to zero by a blackout.
    pub fn is_intensity(self) -> bool {
        matches!(self, AttributeKind::Intensity)
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

const fn attr(name: &'static str, kind: AttributeKind, coarse: u16, fine: Option<u16>, default: f32) -> AttributeDef {
    AttributeDef { name, kind, coarse, fine, default }
}

/// Fixture library. Ships generic profiles; GDTF / OFL importers can
/// register more definitions at runtime via [`FixtureLibrary::register`].
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

#[derive(Debug, Clone)]
struct PatchEntry {
    fixture: PatchedFixture,
    mode: FixtureMode,
}

/// Validated patch: fixture → address resolution.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    entries: HashMap<FixtureId, PatchEntry>,
}

impl Patch {
    /// Builds the patch, reporting unknown definitions, out-of-range
    /// addresses and overlapping footprints (all problems, not just the first).
    pub fn build(fixtures: &[PatchedFixture], library: &FixtureLibrary) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut entries = HashMap::new();
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
            entries.insert(f.id, PatchEntry { fixture: f.clone(), mode: mode.clone() });
        }
        (Self { entries }, errors)
    }

    pub fn resolve(&self, fixture: FixtureId, attribute: &str) -> Option<ResolvedAttribute> {
        let e = self.entries.get(&fixture)?;
        let a = e.mode.attributes.iter().find(|a| a.name.eq_ignore_ascii_case(attribute))?;
        let base = e.fixture.address - 1;
        Some(ResolvedAttribute {
            universe: e.fixture.universe,
            coarse: base + a.coarse + 1,
            fine: a.fine.map(|f| base + f + 1),
            kind: a.kind,
            default: a.default,
        })
    }

    /// All attributes of all fixtures (for homing / blackout classification).
    pub fn all_attributes(&self) -> impl Iterator<Item = ResolvedAttribute> + '_ {
        self.entries.values().flat_map(|e| {
            let base = e.fixture.address - 1;
            e.mode.attributes.iter().map(move |a| ResolvedAttribute {
                universe: e.fixture.universe,
                coarse: base + a.coarse + 1,
                fine: a.fine.map(|f| base + f + 1),
                kind: a.kind,
                default: a.default,
            })
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
}
