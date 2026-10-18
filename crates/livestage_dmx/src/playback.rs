//! LTP (latest-takes-precedence) crossfade playback for DMX cues.
//!
//! Values are tracked per 1-based `(universe, channel)` slot in normalised
//! floating point so 16-bit attributes crossfade smoothly; coarse/fine bytes
//! are only split when rendering.

use crate::fixture::{AttributeKind, Patch};
use crate::universe::Universe;
use livestage_core::action::{DmxChannelValue, FixtureAttributeValue};
use livestage_core::id::CueId;
use livestage_core::time::ShowTime;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot {
    pub universe: u16,
    pub channel: u16,
}

#[derive(Debug, Clone)]
struct ChannelState {
    fine: Option<u16>,
    kind: AttributeKind,
    home: f32,
    from: f32,
    to: f32,
    start: ShowTime,
    duration: ShowTime,
    owner: Option<CueId>,
}

impl ChannelState {
    fn value_at(&self, now: ShowTime) -> f32 {
        if self.duration.is_zero() || now >= self.start + self.duration {
            return self.to;
        }
        let t = (now - self.start).as_secs_f32() / self.duration.as_secs_f32();
        self.from + (self.to - self.from) * t.clamp(0.0, 1.0)
    }
}

#[derive(Debug, Default)]
pub struct DmxPlayback {
    channels: BTreeMap<Slot, ChannelState>,
    /// Which slots each cue currently owns.
    owners: HashMap<CueId, Vec<Slot>>,
}

impl DmxPlayback {
    #[allow(clippy::too_many_arguments)]
    fn fade_slot(
        &mut self,
        now: ShowTime,
        slot: Slot,
        fine: Option<u16>,
        kind: AttributeKind,
        home: f32,
        to: f32,
        fade: ShowTime,
        owner: Option<CueId>,
    ) {
        let st = self.channels.entry(slot).or_insert(ChannelState {
            fine,
            kind,
            home,
            from: home,
            to: home,
            start: now,
            duration: ShowTime::ZERO,
            owner: None,
        });
        st.from = st.value_at(now);
        st.to = to.clamp(0.0, 1.0);
        st.start = now;
        st.duration = fade;
        if let Some(prev) = std::mem::replace(&mut st.owner, owner) {
            if Some(prev) != owner {
                if let Some(v) = self.owners.get_mut(&prev) {
                    v.retain(|s| *s != slot);
                }
            }
        }
        if let Some(o) = owner {
            let list = self.owners.entry(o).or_default();
            if !list.contains(&slot) {
                list.push(slot);
            }
        }
    }

    pub fn apply_scene(
        &mut self,
        now: ShowTime,
        cue: CueId,
        channels: &[DmxChannelValue],
        attributes: &[FixtureAttributeValue],
        fade: ShowTime,
        patch: &Patch,
    ) {
        for c in channels {
            let slot = Slot { universe: c.universe, channel: c.channel };
            // Raw channels are treated as intensity for blackout purposes.
            self.fade_slot(now, slot, None, AttributeKind::Intensity, 0.0, c.value as f32 / 255.0, fade, Some(cue));
        }
        for a in attributes {
            match patch.resolve(a.fixture, &a.attribute) {
                Some(r) => {
                    let slot = Slot { universe: r.universe, channel: r.coarse };
                    self.fade_slot(now, slot, r.fine, r.kind, r.default, a.value, fade, Some(cue));
                }
                None => tracing::warn!(attribute = %a.attribute, "unpatched fixture attribute ignored"),
            }
        }
    }

    /// Fades every slot owned by `cue` back to its home value.
    pub fn release(&mut self, now: ShowTime, cue: CueId, fade: ShowTime) {
        for slot in self.owners.remove(&cue).unwrap_or_default() {
            if let Some(st) = self.channels.get(&slot).cloned() {
                self.fade_slot(now, slot, st.fine, st.kind, st.home, st.home, fade, None);
            }
        }
    }

    /// Fades all intensity slots to zero (positions / colours hold).
    pub fn blackout(&mut self, now: ShowTime, fade: ShowTime) {
        let slots: Vec<(Slot, ChannelState)> =
            self.channels.iter().filter(|(_, s)| s.kind.is_intensity()).map(|(k, v)| (*k, v.clone())).collect();
        for (slot, st) in slots {
            self.fade_slot(now, slot, st.fine, st.kind, st.home, 0.0, fade, None);
        }
        self.owners.clear();
    }

    /// Writes current values into the universes (keyed by internal index).
    pub fn render(&self, now: ShowTime, universes: &mut BTreeMap<u16, Universe>) {
        for (slot, st) in &self.channels {
            let Some(u) = universes.get_mut(&slot.universe) else { continue };
            let v = st.value_at(now);
            match st.fine {
                Some(fine) => {
                    let v16 = (v * 65_535.0).round() as u16;
                    u.set(slot.channel, (v16 >> 8) as u8);
                    u.set(fine, (v16 & 0xFF) as u8);
                }
                None => u.set(slot.channel, (v * 255.0).round() as u8),
            }
        }
    }

    pub fn is_fading(&self, now: ShowTime) -> bool {
        self.channels.values().any(|s| !s.duration.is_zero() && now < s.start + s.duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossfade_and_release() {
        let patch = Patch::default();
        let mut pb = DmxPlayback::default();
        let mut unis = BTreeMap::from([(0u16, Universe::default())]);
        let cue = CueId::new();
        let ch = [DmxChannelValue { universe: 0, channel: 1, value: 255 }];
        pb.apply_scene(ShowTime::ZERO, cue, &ch, &[], ShowTime::from_secs(2), &patch);
        pb.render(ShowTime::from_secs(1), &mut unis);
        assert_eq!(unis[&0].get(1), 128);
        pb.render(ShowTime::from_secs(3), &mut unis);
        assert_eq!(unis[&0].get(1), 255);
        pb.release(ShowTime::from_secs(3), cue, ShowTime::ZERO);
        pb.render(ShowTime::from_secs(3), &mut unis);
        assert_eq!(unis[&0].get(1), 0);
    }
}
