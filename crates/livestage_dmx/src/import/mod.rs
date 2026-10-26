//! Fixture profile import: GDTF (`.gdtf`) and Open Fixture Library (`.json`).
//!
//! Both formats are converted to a [`FixtureProfile`] that is stored in the
//! show and turned into a library definition by
//! [`FixtureDefinition::from_profile`](crate::fixture::FixtureDefinition::from_profile).
//!
//! Attributes LiveStage understands get the same names as the built-in
//! types (`intensity`, `red`, `pan`, `gobo`, ...), so palettes, effects and
//! the programmer work on imported fixtures unchanged. A second attribute of
//! the same kind (a second gobo wheel, the cells of an LED bar) is numbered:
//! `gobo 2`, `red 2`, `red 3`. Everything else keeps its own name, lower
//! case, as a generic attribute.

mod gdtf;
mod ofl;

use crate::fixture::AttributeKind;
use livestage_core::dmx_config::{FixtureProfile, ProfileAttribute};
use std::collections::HashMap;
use std::path::Path;

/// An imported profile and what could not be imported.
#[derive(Debug, Clone)]
pub struct Imported {
    pub profile: FixtureProfile,
    /// Skipped modes or channels, for the operator.
    pub warnings: Vec<String>,
}

/// Imports a `.gdtf` file or an Open Fixture Library `.json` file. OFL files
/// do not name their manufacturer: it is taken from the folder the file is
/// in (`fixtures/<manufacturer>/<fixture>.json` in the library).
pub fn import_file(path: &Path) -> Result<Imported, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "gdtf" => gdtf::parse(&bytes),
        "json" => {
            let key = |p: Option<&Path>| p.and_then(|p| p.file_stem()).and_then(|s| s.to_str()).unwrap_or_default().to_string();
            ofl::parse(&bytes, &key(path.parent()), &key(Some(path)))
        }
        _ => Err(format!("{}: not a .gdtf or OFL .json fixture file", path.display())),
    }
}

/// Lower-case key part: letters and digits, words joined by `-`.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// `cameo` → `Cameo`, `chauvet-dj` → `Chauvet Dj`.
fn title(key: &str) -> String {
    key.split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hands out attribute names, unique within a mode.
#[derive(Default)]
struct Namer {
    used: HashMap<String, u32>,
}

impl Namer {
    /// Known kinds take the kind's name, others their own (lower case);
    /// repeats are numbered from 2.
    fn name(&mut self, kind: AttributeKind, own: &str) -> String {
        let base = match kind {
            AttributeKind::Generic | AttributeKind::Control => own.trim().to_lowercase(),
            k => k.name().to_string(),
        };
        let base = if base.is_empty() { kind.name().to_string() } else { base };
        let n = self.used.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 {
            base
        } else {
            format!("{base} {n}")
        }
    }
}

fn attribute(namer: &mut Namer, kind: AttributeKind, own: &str, coarse: u16, fine: Option<u16>, default: f32) -> ProfileAttribute {
    ProfileAttribute { name: namer.name(kind, own), kind: kind.name().into(), coarse, fine, default: default.clamp(0.0, 1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_canonical_and_unique() {
        let mut n = Namer::default();
        assert_eq!(n.name(AttributeKind::Red, "Red"), "red");
        assert_eq!(n.name(AttributeKind::Red, "ColorAdd_R"), "red 2");
        assert_eq!(n.name(AttributeKind::Generic, "Prism Rotation"), "prism rotation");
        assert_eq!(n.name(AttributeKind::Generic, "Prism Rotation"), "prism rotation 2");
        assert_eq!(slug("Robe Lighting s.r.o."), "robe-lighting-s-r-o");
        assert_eq!(title("chauvet-dj"), "Chauvet Dj");
    }
}
