//! DMX engine: executes core `DmxAction`s, renders playback + pixel maps.

use crate::fixture::{FixtureLibrary, Patch};
use crate::output::{DmxOutput, SharedFrame};
use crate::pixelmap::{self, RgbaView};
use crate::playback::DmxPlayback;
use crate::universe::Universe;
use livestage_core::action::DmxAction;
use livestage_core::dmx_config::DmxConfig;
use livestage_core::id::OutputId;
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
        }
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

    pub fn handle(&mut self, now: ShowTime, action: &DmxAction) {
        match action {
            DmxAction::ApplyScene { cue, channels, attributes, fade } => {
                self.playback.apply_scene(now, *cue, channels, attributes, *fade, &self.patch)
            }
            DmxAction::Release { cue, fade } => self.playback.release(now, *cue, *fade),
            DmxAction::Blackout { fade } => self.playback.blackout(now, *fade),
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

    /// Renders playback into the universes and publishes the frame to the sender.
    pub fn tick(&mut self, now: ShowTime) {
        self.playback.render(now, &mut self.universes);
        let mut f = self.frame.lock();
        f.clone_from(&self.universes);
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
