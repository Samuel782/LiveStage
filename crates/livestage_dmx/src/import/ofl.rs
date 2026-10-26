//! Open Fixture Library (<https://open-fixture-library.org>) fixture JSON.
//!
//! `availableChannels` describes every channel once (its capabilities, fine
//! channel aliases and default); each mode lists channel names by DMX slot,
//! `null` for unused slots. Matrix modes (`insert: matrixChannels`) are not
//! supported yet and are skipped with a warning.

use super::{attribute, slug, title, Imported, Namer};
use crate::fixture::AttributeKind;
use livestage_core::dmx_config::{FixtureProfile, ProfileMode, ProfileSource};
use serde_json::Value;
use std::collections::HashMap;

pub(super) fn parse(bytes: &[u8], manufacturer_key: &str, fixture_key: &str) -> Result<Imported, String> {
    let doc: Value = serde_json::from_slice(bytes).map_err(|e| format!("not an OFL fixture: {e}"))?;
    let model = doc.get("name").and_then(Value::as_str).ok_or("not an OFL fixture: no `name`")?.to_string();
    let channels = doc.get("availableChannels").and_then(Value::as_object).ok_or("not an OFL fixture: no `availableChannels`")?;
    let modes = doc.get("modes").and_then(Value::as_array).ok_or("not an OFL fixture: no `modes`")?;

    // Fine aliases → (channel, byte); switching-channel aliases → channel.
    let mut fine_of: HashMap<&str, (&str, usize)> = HashMap::new();
    let mut switched: HashMap<&str, &str> = HashMap::new();
    for (name, ch) in channels {
        for (i, alias) in ch.get("fineChannelAliases").and_then(Value::as_array).into_iter().flatten().enumerate() {
            if let Some(a) = alias.as_str() {
                fine_of.insert(a, (name.as_str(), i + 1));
            }
        }
        for cap in capabilities(ch) {
            for (alias, target) in cap.get("switchChannels").and_then(Value::as_object).into_iter().flatten() {
                if let Some(t) = target.as_str() {
                    switched.entry(alias.as_str()).or_insert(t);
                }
            }
        }
    }

    let mut warnings = Vec::new();
    let mut out_modes = Vec::new();
    for mode in modes {
        let mode_name = mode.get("shortName").or_else(|| mode.get("name")).and_then(Value::as_str).unwrap_or("Default").to_string();
        let Some(slots) = mode.get("channels").and_then(Value::as_array) else { continue };
        if slots.iter().any(|s| !(s.is_string() || s.is_null())) {
            warnings.push(format!("mode {mode_name}: matrix modes are not supported yet; skipped"));
            continue;
        }
        let slots: Vec<Option<&str>> = slots.iter().map(Value::as_str).collect();
        let position = |name: &str| slots.iter().position(|s| *s == Some(name)).map(|p| p as u16);
        let mut namer = Namer::default();
        let mut attributes = Vec::new();
        for (offset, slot) in slots.iter().enumerate() {
            let Some(name) = *slot else { continue };
            if let Some((base, byte)) = fine_of.get(name) {
                // The 16-bit fine byte belongs to its coarse channel (below);
                // a fine byte without its coarse one in this mode, or a third
                // byte, stays addressable on its own.
                if *byte == 1 && position(base).is_some() {
                    continue;
                }
            }
            let resolved = switched.get(name).copied().unwrap_or(name);
            let (kind, default, fine) = match channels.get(resolved) {
                Some(ch) => {
                    let fine = ch
                        .get("fineChannelAliases")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(Value::as_str)
                        .and_then(position);
                    (kind_of(resolved, ch), default_of(ch, fine.is_some()), fine)
                }
                None => {
                    warnings.push(format!("mode {mode_name}: channel `{name}` is not described; imported as generic"));
                    (AttributeKind::Generic, 0.0, None)
                }
            };
            attributes.push(attribute(&mut namer, kind, name, offset as u16, fine, default));
        }
        out_modes.push(ProfileMode { name: mode_name, footprint: slots.len() as u16, attributes });
    }
    if out_modes.is_empty() {
        return Err(format!("{model}: no supported modes"));
    }
    let manufacturer = doc.get("manufacturer").and_then(Value::as_str).map_or_else(|| title(manufacturer_key), str::to_string);
    let manufacturer = if manufacturer.is_empty() { "Unknown".to_string() } else { manufacturer };
    let fixture_key = if fixture_key.is_empty() { slug(&model) } else { slug(fixture_key) };
    let profile = FixtureProfile {
        key: format!("ofl/{}/{fixture_key}", slug(&manufacturer)),
        manufacturer,
        model,
        source: ProfileSource::Ofl,
        modes: out_modes,
    };
    Ok(Imported { profile, warnings })
}

fn capabilities(ch: &Value) -> impl Iterator<Item = &Value> {
    ch.get("capability").into_iter().chain(ch.get("capabilities").and_then(Value::as_array).into_iter().flatten())
}

/// The kind of the first capability that does something (many channels
/// start with a "no function" range, e.g. shutter open before the strobe).
fn kind_of(name: &str, ch: &Value) -> AttributeKind {
    use AttributeKind::*;
    for cap in capabilities(ch) {
        let ty = cap.get("type").and_then(Value::as_str).unwrap_or_default();
        let kind = match ty {
            "Intensity" => Intensity,
            "ColorIntensity" => match cap.get("color").and_then(Value::as_str).unwrap_or_default() {
                "Red" => Red,
                "Green" => Green,
                "Blue" => Blue,
                "White" | "Warm White" | "Cold White" => White,
                "Amber" => Amber,
                "UV" => Uv,
                _ => Generic,
            },
            "Pan" | "PanContinuous" => Pan,
            "Tilt" | "TiltContinuous" => Tilt,
            "Zoom" => Zoom,
            "Focus" => Focus,
            "ShutterStrobe" => Shutter,
            "WheelSlot" => {
                let wheel = cap.get("wheel").and_then(Value::as_str).unwrap_or(name).to_lowercase();
                if wheel.contains("gobo") {
                    Gobo
                } else if wheel.contains("colo") {
                    ColorWheel
                } else {
                    Generic
                }
            }
            "Maintenance" => Control,
            "NoFunction" | "Generic" => continue,
            _ => Generic,
        };
        return kind;
    }
    Generic
}

/// `defaultValue`: a DMX value (8-bit, or 16-bit for channels with a fine
/// byte) or a percentage string.
fn default_of(ch: &Value, sixteen_bit: bool) -> f32 {
    match ch.get("defaultValue") {
        Some(Value::Number(n)) => {
            let v = n.as_f64().unwrap_or(0.0);
            let max = if sixteen_bit && v > 255.0 { 65_535.0 } else { 255.0 };
            (v / max) as f32
        }
        Some(Value::String(s)) => s.trim().trim_end_matches('%').trim().parse::<f32>().map_or(0.0, |p| p / 100.0),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use crate::fixture::{FixtureDefinition, FixtureLibrary, Patch};
    use livestage_core::dmx_config::PatchedFixture;
    use std::path::Path;

    fn testdata(rel: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/ofl").join(rel)
    }

    /// Real library files: a moving head with fine channels, wheels and
    /// percentage defaults, and an RGBAW+UV par with a switching channel.
    #[test]
    fn imports_real_ofl_fixtures() {
        let spot = crate::import::import_file(&testdata("cameo/auro-spot-300.json")).unwrap();
        let p = &spot.profile;
        assert_eq!((p.key.as_str(), p.manufacturer.as_str(), p.model.as_str()), ("ofl/cameo/auro-spot-300", "Cameo", "Auro Spot 300"));
        assert_eq!(p.modes.iter().map(|m| (m.name.as_str(), m.footprint)).collect::<Vec<_>>(), [("5ch", 5), ("15ch", 15), ("24ch", 24)]);
        let m = &p.modes[1];
        let get = |n: &str| m.attributes.iter().find(|a| a.name == n).unwrap_or_else(|| panic!("no {n} in {:?}", m.attributes));
        assert_eq!((get("pan").coarse, get("pan").fine, get("pan").default), (0, Some(1), 0.5));
        assert_eq!((get("tilt").coarse, get("tilt").fine), (2, Some(3)));
        assert_eq!((get("intensity").coarse, get("intensity").fine), (4, None), "no fine dimmer in 15ch");
        assert_eq!(get("shutter").kind, "shutter");
        assert_eq!(get("color").kind, "color");
        assert_eq!((get("gobo").coarse, get("gobo 2").coarse), (7, 10));
        assert_eq!(get("gobo 1 rotation").kind, "generic");
        assert_eq!(get("focus").coarse, 13);
        assert_eq!(m.attributes.len(), 13, "15 slots, two of them fine bytes");
        let dimmer24 = p.modes[2].attributes.iter().find(|a| a.name == "intensity").unwrap();
        assert_eq!((dimmer24.coarse, dimmer24.fine), (4, Some(5)));

        let par = crate::import::import_file(&testdata("chauvet-dj/slimpar-pro-h-usb.json")).unwrap();
        let m12 = par.profile.modes.iter().find(|m| m.name == "12ch").unwrap();
        let names: Vec<&str> = m12.attributes.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(&names[..7], ["intensity", "red", "green", "blue", "amber", "white", "uv"]);
        assert!(names.contains(&"program speed / sound sensitivity"), "{names:?}");
        assert!(par.warnings.is_empty(), "{:?}", par.warnings);

        // Every mode is a valid definition, and patches.
        let (lib, errors) = FixtureLibrary::with_profiles(&[spot.profile.clone(), par.profile.clone()]);
        assert!(errors.is_empty(), "{errors:?}");
        FixtureDefinition::from_profile(&par.profile).unwrap();
        let f = PatchedFixture {
            id: Default::default(),
            name: "Spot".into(),
            definition: spot.profile.key.clone(),
            mode: "24ch".into(),
            universe: 0,
            address: 1,
        };
        let (patch, errors) = Patch::build(std::slice::from_ref(&f), &lib);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(patch.resolve(f.id, "pan").map(|r| (r.coarse, r.fine)), Some((1, Some(2))));
    }

    #[test]
    fn rejects_files_that_are_not_fixtures() {
        assert!(super::parse(b"{\"name\": 3}", "x", "y").is_err());
        assert!(super::parse(b"not json", "x", "y").is_err());
        let matrix = br#"{"name":"Bar","availableChannels":{"Red":{"capability":{"type":"ColorIntensity","color":"Red"}}},
            "modes":[{"name":"Pixels","channels":[{"insert":"matrixChannels"}]},{"name":"1ch","channels":["Red"]}]}"#;
        let i = super::parse(matrix, "acme", "bar").unwrap();
        assert_eq!(i.profile.modes.len(), 1);
        assert_eq!(i.warnings.len(), 1);
        assert_eq!(i.profile.key, "ofl/acme/bar");
    }
}
