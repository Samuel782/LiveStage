//! Compiled routing table used by the real-time thread.

use crate::db_to_gain;
use livestage_core::id::StemId;
use livestage_core::routing::AudioRouting;

#[derive(Debug, Clone)]
pub struct CompiledStem {
    pub id: StemId,
    pub channels: u16,
    pub gain: f32,
    /// `(stem source channel, physical output, linear gain)`.
    pub routes: Vec<(u16, u16, f32)>,
}

/// Flattened, allocation-free-to-read form of [`AudioRouting`].
#[derive(Debug, Clone, Default)]
pub struct RoutingTable {
    pub stems: Vec<CompiledStem>,
}

impl RoutingTable {
    /// Compiles the operator patch for a device with `device_channels` outputs.
    /// Crosspoints beyond the device's channel count are dropped and listed in
    /// the second return value as `(stem name, 1-based output)`.
    pub fn compile(routing: &AudioRouting, device_channels: u16) -> (Self, Vec<(String, u16)>) {
        let mut dropped = Vec::new();
        let stems = routing
            .stems
            .iter()
            .map(|s| {
                let routes = routing
                    .crosspoints
                    .iter()
                    .filter(|c| c.stem == s.id && c.source_channel < s.channels.max(1))
                    .filter(|c| {
                        let ok = c.output < device_channels;
                        if !ok {
                            dropped.push((s.name.clone(), c.output + 1));
                        }
                        ok
                    })
                    .map(|c| (c.source_channel, c.output, db_to_gain(c.gain_db)))
                    .collect();
                CompiledStem {
                    id: s.id,
                    channels: s.channels.max(1),
                    gain: if s.mute { 0.0 } else { db_to_gain(s.gain_db) },
                    routes,
                }
            })
            .collect();
        (Self { stems }, dropped)
    }

    #[inline]
    pub fn stem(&self, id: StemId) -> Option<&CompiledStem> {
        self.stems.iter().find(|s| s.id == id)
    }
}
