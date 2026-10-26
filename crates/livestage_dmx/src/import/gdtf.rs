//! GDTF fixture types (DIN SPEC 15800): a zip archive whose
//! `description.xml` holds the fixture type, its geometries and DMX modes.
//!
//! Each `DMXChannel` gives its byte offsets (`Offset="1,2"`: coarse and fine,
//! 1-based) and its attribute (`LogicalChannel@Attribute`). Channels of a
//! geometry instanced by `GeometryReference`s (the cells of an LED bar) are
//! repeated once per reference at the reference's `Break@DMXOffset`.
//! Virtual channels (no offset) have no DMX slot and are left out, as are
//! channels on a second DMX break, which LiveStage does not patch yet.

use super::{attribute, slug, Imported, Namer};
use crate::fixture::AttributeKind;
use livestage_core::dmx_config::{FixtureProfile, ProfileMode, ProfileSource};
use roxmltree::{Document, Node};
use std::io::Read;

pub(super) fn parse(bytes: &[u8]) -> Result<Imported, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not a GDTF file (zip): {e}"))?;
    let name = archive
        .file_names()
        .find(|n| n.eq_ignore_ascii_case("description.xml"))
        .map(str::to_string)
        .ok_or("not a GDTF file: no description.xml")?;
    let mut xml = String::new();
    archive
        .by_name(&name)
        .map_err(|e| e.to_string())?
        .read_to_string(&mut xml)
        .map_err(|e| format!("description.xml: {e}"))?;
    parse_description(&xml)
}

fn parse_description(xml: &str) -> Result<Imported, String> {
    let doc = Document::parse(xml).map_err(|e| format!("description.xml: {e}"))?;
    let ft = doc.descendants().find(|n| n.has_tag_name("FixtureType")).ok_or("description.xml: no FixtureType")?;
    let model = ft.attribute("LongName").filter(|s| !s.is_empty()).or(ft.attribute("Name")).unwrap_or("Unnamed").to_string();
    let manufacturer = ft.attribute("Manufacturer").filter(|s| !s.is_empty()).unwrap_or("Unknown").to_string();

    // (referenced geometry, reference name, 1-based offset on break 1)
    let references: Vec<(&str, &str, u16)> = ft
        .descendants()
        .filter(|n| n.has_tag_name("GeometryReference"))
        .filter_map(|r| {
            let brk = r.children().filter(|b| b.has_tag_name("Break")).find(|b| b.attribute("DMXBreak").is_none_or(|v| v.trim() == "1"))?;
            Some((r.attribute("Geometry")?, r.attribute("Name").unwrap_or_default(), brk.attribute("DMXOffset")?.trim().parse().ok()?))
        })
        .collect();

    let mut warnings = Vec::new();
    let mut modes = Vec::new();
    for mode in ft.descendants().filter(|n| n.has_tag_name("DMXMode")) {
        let mode_name = mode.attribute("Name").unwrap_or("Default").to_string();
        // (attribute, 1-based offsets, default, own name)
        let mut channels: Vec<(String, Vec<u16>, f32)> = Vec::new();
        let (mut virtual_channels, mut other_breaks) = (0, 0);
        for ch in mode.descendants().filter(|n| n.has_tag_name("DMXChannel")) {
            let offsets: Vec<u16> = ch.attribute("Offset").unwrap_or_default().split(',').filter_map(|o| o.trim().parse().ok()).collect();
            if offsets.is_empty() {
                virtual_channels += 1;
                continue;
            }
            let brk = ch.attribute("DMXBreak").unwrap_or("1").trim();
            let attr = channel_attribute(ch);
            let default = channel_default(ch);
            let geometry = ch.attribute("Geometry").unwrap_or_default();
            let instances: Vec<_> = references.iter().filter(|(g, _, _)| *g == geometry).collect();
            if !instances.is_empty() {
                // A template channel: one copy per geometry reference.
                for (_, _, base) in instances {
                    channels.push((attr.clone(), offsets.iter().map(|o| base + o - 1).collect(), default));
                }
            } else if brk == "1" || brk.eq_ignore_ascii_case("Overwrite") {
                channels.push((attr, offsets, default));
            } else {
                other_breaks += 1;
            }
        }
        if other_breaks > 0 {
            warnings.push(format!("mode {mode_name}: {other_breaks} channel(s) on a second DMX break were left out"));
        }
        if virtual_channels > 0 {
            tracing::debug!(mode = %mode_name, virtual_channels, "GDTF virtual channels have no DMX slot");
        }
        channels.sort_by_key(|(_, offsets, _)| offsets[0]);
        let footprint = channels.iter().flat_map(|(_, o, _)| o.iter().copied()).max().unwrap_or(0);
        if footprint == 0 || footprint > 512 {
            warnings.push(format!("mode {mode_name}: no DMX channels; skipped"));
            continue;
        }
        let mut namer = Namer::default();
        let attributes = channels
            .iter()
            .map(|(attr, offsets, default)| {
                let kind = kind_of(attr);
                attribute(&mut namer, kind, attr, offsets[0] - 1, offsets.get(1).map(|f| f - 1), *default)
            })
            .collect();
        modes.push(ProfileMode { name: mode_name, footprint, attributes });
    }
    if modes.is_empty() {
        return Err(format!("{model}: no DMX modes"));
    }
    let profile = FixtureProfile {
        key: format!("gdtf/{}/{}", slug(&manufacturer), slug(&model)),
        manufacturer,
        model,
        source: ProfileSource::Gdtf,
        modes,
    };
    Ok(Imported { profile, warnings })
}

/// The attribute a channel controls: its first logical channel's, else the
/// one named in `InitialFunction` (`Geometry_Attribute.Attribute.Function`).
fn channel_attribute(ch: Node) -> String {
    ch.children()
        .find(|n| n.has_tag_name("LogicalChannel"))
        .and_then(|l| l.attribute("Attribute"))
        .or_else(|| ch.attribute("InitialFunction").and_then(|f| f.split('.').nth(1)))
        .unwrap_or("Generic")
        .to_string()
}

/// Home value: the channel's (GDTF 1.0) or its first channel function's
/// `Default`, written `value/bytes` (`32768/2` = half of a 16-bit range).
fn channel_default(ch: Node) -> f32 {
    let raw = ch.attribute("Default").or_else(|| {
        ch.descendants().find(|n| n.has_tag_name("ChannelFunction")).and_then(|f| f.attribute("Default"))
    });
    let Some(raw) = raw else { return 0.0 };
    let (value, bytes) = match raw.split_once('/') {
        Some((v, b)) => (v.trim().parse::<f64>().unwrap_or(0.0), b.trim().parse::<i32>().unwrap_or(1).clamp(1, 4)),
        None => (raw.trim().parse::<f64>().unwrap_or(0.0), 1),
    };
    (value / (2f64.powi(8 * bytes) - 1.0)) as f32
}

/// GDTF attribute names (the standard list in DIN SPEC 15800 annex A).
fn kind_of(attr: &str) -> AttributeKind {
    use AttributeKind::*;
    let a = attr.to_ascii_lowercase();
    let numbered = |prefix: &str| a.strip_prefix(prefix).is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()));
    match a.as_str() {
        "dimmer" => Intensity,
        "pan" => Pan,
        "tilt" => Tilt,
        "coloradd_r" | "colorrgb_red" => Red,
        "coloradd_g" | "colorrgb_green" => Green,
        "coloradd_b" | "colorrgb_blue" => Blue,
        "coloradd_w" | "coloradd_ww" | "coloradd_cw" | "colorrgb_white" => White,
        "coloradd_a" | "colorrgb_amber" => Amber,
        "coloradd_uv" | "colorrgb_uv" => Uv,
        "zoom" => Zoom,
        "focus" => Focus,
        "control" | "reset" | "lampcontrol" | "function" | "displayintensity" => Control,
        _ if numbered("color") => ColorWheel,
        _ if numbered("gobo") => Gobo,
        _ if numbered("focus") => Focus,
        _ if numbered("shutter") || a.starts_with("shutter") && a.ends_with("strobe") => Shutter,
        _ if numbered("control") || a.ends_with("reset") => Control,
        _ => Generic,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    /// A GDTF 1.1 moving head (16-bit pan/tilt, virtual dimmer channel,
    /// a channel on a second break) and a 4-cell RGB bar built from
    /// geometry references.
    const DESCRIPTION: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<GDTF DataVersion="1.1">
  <FixtureType Name="Spot" LongName="Test Spot 300" ShortName="TS300" Manufacturer="Acme Lighting" FixtureTypeID="00000000-0000-0000-0000-000000000001">
    <Geometries>
      <Geometry Name="Base">
        <Axis Name="Yoke"><Axis Name="Head"><Beam Name="Beam"/></Axis></Axis>
        <GeometryReference Name="Cell 1" Geometry="Cell"><Break DMXBreak="1" DMXOffset="3"/></GeometryReference>
        <GeometryReference Name="Cell 2" Geometry="Cell"><Break DMXBreak="1" DMXOffset="6"/></GeometryReference>
      </Geometry>
      <Geometry Name="Cell"><Beam Name="Pixel"/></Geometry>
    </Geometries>
    <DMXModes>
      <DMXMode Name="Standard" Geometry="Base">
        <DMXChannels>
          <DMXChannel DMXBreak="1" Offset="1,2" Geometry="Yoke" InitialFunction="Yoke_Pan.Pan.Pan 1">
            <LogicalChannel Attribute="Pan"><ChannelFunction Name="Pan 1" Attribute="Pan" DMXFrom="0/1" Default="32768/2"/></LogicalChannel>
          </DMXChannel>
          <DMXChannel DMXBreak="1" Offset="3,4" Geometry="Head">
            <LogicalChannel Attribute="Tilt"><ChannelFunction Name="Tilt 1" Attribute="Tilt" Default="32768/2"/></LogicalChannel>
          </DMXChannel>
          <DMXChannel DMXBreak="1" Offset="5" Geometry="Beam">
            <LogicalChannel Attribute="Dimmer"><ChannelFunction Attribute="Dimmer" Default="0/1"/></LogicalChannel>
          </DMXChannel>
          <DMXChannel DMXBreak="1" Offset="6" Geometry="Beam" InitialFunction="Beam_Shutter1.Shutter1.Open">
            <LogicalChannel Attribute="Shutter1"><ChannelFunction Attribute="Shutter1" Default="255/1"/></LogicalChannel>
          </DMXChannel>
          <DMXChannel DMXBreak="1" Offset="7" Geometry="Beam"><LogicalChannel Attribute="Gobo1"/></DMXChannel>
          <DMXChannel DMXBreak="1" Offset="8" Geometry="Beam"><LogicalChannel Attribute="Gobo2"/></DMXChannel>
          <DMXChannel DMXBreak="1" Offset="9" Geometry="Beam"><LogicalChannel Attribute="Prism1"/></DMXChannel>
          <DMXChannel DMXBreak="1" Offset="None" Geometry="Beam"><LogicalChannel Attribute="Dimmer"/></DMXChannel>
          <DMXChannel DMXBreak="2" Offset="1" Geometry="Beam"><LogicalChannel Attribute="Control1"/></DMXChannel>
        </DMXChannels>
      </DMXMode>
      <DMXMode Name="Bar" Geometry="Base">
        <DMXChannels>
          <DMXChannel DMXBreak="1" Offset="1" Geometry="Beam"><LogicalChannel Attribute="Dimmer"/></DMXChannel>
          <DMXChannel DMXBreak="1" Offset="2" Geometry="Beam"><LogicalChannel Attribute="Shutter1Strobe"/></DMXChannel>
          <DMXChannel DMXBreak="Overwrite" Offset="1" Geometry="Cell"><LogicalChannel Attribute="ColorAdd_R"/></DMXChannel>
          <DMXChannel DMXBreak="Overwrite" Offset="2" Geometry="Cell"><LogicalChannel Attribute="ColorAdd_G"/></DMXChannel>
          <DMXChannel DMXBreak="Overwrite" Offset="3" Geometry="Cell"><LogicalChannel Attribute="ColorAdd_B"/></DMXChannel>
        </DMXChannels>
      </DMXMode>
    </DMXModes>
  </FixtureType>
</GDTF>"#;

    fn gdtf_file(xml: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("description.xml", opts).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.start_file("thumbnail.png", opts).unwrap();
        zip.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn imports_modes_offsets_defaults_and_cells() {
        let i = super::parse(&gdtf_file(DESCRIPTION)).unwrap();
        let p = &i.profile;
        assert_eq!((p.key.as_str(), p.manufacturer.as_str(), p.model.as_str()), ("gdtf/acme-lighting/test-spot-300", "Acme Lighting", "Test Spot 300"));
        let std = &p.modes[0];
        assert_eq!((std.name.as_str(), std.footprint), ("Standard", 9));
        let get = |n: &str| std.attributes.iter().find(|a| a.name == n).unwrap_or_else(|| panic!("no {n}: {:?}", std.attributes));
        assert_eq!((get("pan").coarse, get("pan").fine), (0, Some(1)));
        assert!((get("pan").default - 0.5).abs() < 1e-4);
        assert_eq!((get("tilt").coarse, get("tilt").fine), (2, Some(3)));
        assert_eq!((get("intensity").coarse, get("intensity").kind.as_str()), (4, "intensity"));
        assert_eq!((get("shutter").default, get("shutter").kind.as_str()), (1.0, "shutter"));
        assert_eq!((get("gobo").coarse, get("gobo 2").coarse), (6, 7));
        assert_eq!(get("prism1").kind, "generic");
        assert_eq!(std.attributes.len(), 7, "virtual and break-2 channels have no slot here");
        assert_eq!(i.warnings.len(), 1, "{:?}", i.warnings);

        let bar = &p.modes[1];
        assert_eq!(bar.footprint, 8, "2 + 2 cells × 3 at offsets 3 and 6");
        let names: Vec<(&str, u16)> = bar.attributes.iter().map(|a| (a.name.as_str(), a.coarse)).collect();
        assert_eq!(names, [("intensity", 0), ("shutter", 1), ("red", 2), ("green", 3), ("blue", 4), ("red 2", 5), ("green 2", 6), ("blue 2", 7)]);
        crate::fixture::FixtureDefinition::from_profile(p).unwrap();
    }

    #[test]
    fn rejects_broken_files() {
        assert!(super::parse(b"plain text").unwrap_err().contains("zip"));
        assert!(super::parse(&gdtf_file("<GDTF><Nothing/></GDTF>")).unwrap_err().contains("FixtureType"));
        assert!(super::parse(&gdtf_file("<GDTF><FixtureType Name='X'>")).is_err());
    }
}
