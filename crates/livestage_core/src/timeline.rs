//! Stacked / timeline cues.
//!
//! A [`TimelineCue`] groups child cues on a time axis. An optional *master*
//! cue (typically an [`AudioCue`](crate::cue::AudioCue)) acts as the clock
//! backbone: children are fired when the master's clock — the audio
//! engine's sample counter, not the UI frame clock — passes their offset.
//! Starting or seeking a timeline mid-way *chases*: children whose window
//! covers the start point are started part-way through.

use crate::cue::*;
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineItem {
    /// Offset from the start of the timeline.
    pub offset: ShowTime,
    pub cue: AnyCue,
    #[serde(skip)]
    pub fired: bool,
}

impl TimelineItem {
    pub fn new(offset: ShowTime, cue: AnyCue) -> Self {
        Self { offset, cue, fired: false }
    }
    pub fn end(&self) -> Option<ShowTime> {
        self.cue.duration().map(|d| self.offset + d)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineCue {
    #[serde(flatten)]
    pub header: CueHeader,
    /// Clock backbone. Without one the timeline runs on the show clock.
    #[serde(default)]
    pub master: Option<Box<AnyCue>>,
    /// Children, kept sorted by offset.
    #[serde(default)]
    pub items: Vec<TimelineItem>,
    /// Explicit length; otherwise derived from master / children.
    #[serde(default)]
    pub length: Option<ShowTime>,
}

impl TimelineCue {
    pub fn new(header: CueHeader, master: Option<AnyCue>) -> Self {
        Self { header, master: master.map(Box::new), items: Vec::new(), length: None }
    }

    /// Inserts a child keeping the list ordered by offset.
    pub fn add(&mut self, offset: ShowTime, cue: AnyCue) {
        let idx = self.items.partition_point(|i| i.offset <= offset);
        self.items.insert(idx, TimelineItem::new(offset, cue));
    }

    fn timeline_clock(&self, ctx: &dyn CueContext) -> Option<ShowTime> {
        let m = self.master.as_deref()?;
        if m.state().is_active() {
            Some(m.elapsed(ctx))
        } else {
            None
        }
    }

    fn fire_due(&mut self, ctx: &mut dyn CueContext, position: ShowTime, chasing: bool) {
        for item in self.items.iter_mut().filter(|i| !i.fired && i.offset <= position) {
            item.fired = true;
            let into = position - item.offset;
            if chasing {
                // Past triggers don't re-fire and finished media is skipped, but
                // DMX looks are always applied so the rig matches the playhead.
                let still_running = item.end().is_none_or(|end| position < end);
                let fire = item.cue.kind() == CueKind::Dmx || (item.cue.chase_on_catch_up() && still_running);
                if !fire {
                    continue;
                }
            }
            // `into` is the lateness: children start already `into` deep, which
            // compensates for UI tick granularity.
            item.cue.go_from(ctx, into);
        }
    }
}

impl Cue for TimelineCue {
    fn header(&self) -> &CueHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut CueHeader {
        &mut self.header
    }
    fn kind(&self) -> CueKind {
        CueKind::Timeline
    }

    fn duration(&self) -> Option<ShowTime> {
        if let Some(l) = self.length {
            return Some(l);
        }
        if let Some(m) = &self.master {
            return m.duration();
        }
        // No master: as long as the last child.
        self.items.iter().map(|i| i.end()).try_fold(ShowTime::ZERO, |acc, e| e.map(|e| acc.max(e)))
    }

    fn prepare(&mut self, ctx: &mut dyn CueContext) {
        if let Some(m) = self.master.as_deref_mut() {
            m.prepare(ctx);
        }
        for i in &mut self.items {
            i.cue.prepare(ctx);
        }
    }

    fn on_go(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) {
        for i in &mut self.items {
            i.fired = false;
            i.cue.reset();
        }
        if let Some(m) = self.master.as_deref_mut() {
            m.go_from(ctx, offset);
        }
        self.fire_due(ctx, offset, !offset.is_zero());
    }

    fn on_seek(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) -> bool {
        let paused = self.header.runtime.state == crate::state::CueState::Paused;
        if let Some(m) = self.master.as_deref_mut() {
            m.seek(ctx, offset);
        }
        for i in &mut self.items {
            let covers = i.offset <= offset && i.end().is_none_or(|end| offset < end);
            if i.cue.state().is_active() {
                if covers {
                    // Already playing and still under the playhead: move it in place.
                    i.cue.seek(ctx, offset - i.offset);
                    i.fired = true;
                    continue;
                }
                // DMX looks track; everything else outside the playhead stops.
                if i.cue.kind() != CueKind::Dmx {
                    i.cue.stop(ctx, Some(ShowTime::ZERO));
                }
            }
            i.fired = false;
            if !i.cue.state().is_active() {
                i.cue.reset();
            }
        }
        // Chase whatever the new playhead covers.
        self.fire_due(ctx, offset, true);
        if paused {
            for i in self.items.iter_mut().filter(|i| i.cue.state() == crate::state::CueState::Running) {
                i.cue.pause(ctx);
            }
        }
        true
    }

    fn on_tick(&mut self, ctx: &mut dyn CueContext, elapsed: ShowTime) {
        if let Some(m) = self.master.as_deref_mut() {
            m.tick(ctx);
        }
        self.fire_due(ctx, elapsed, false);
        for i in self.items.iter_mut().filter(|i| i.cue.state().is_active()) {
            i.cue.tick(ctx);
        }
    }

    fn on_stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        if let Some(m) = self.master.as_deref_mut() {
            m.stop(ctx, fade);
        }
        for i in &mut self.items {
            i.cue.stop(ctx, fade);
        }
    }

    fn on_pause(&mut self, ctx: &mut dyn CueContext) {
        if let Some(m) = self.master.as_deref_mut() {
            m.pause(ctx);
        }
        for i in &mut self.items {
            i.cue.pause(ctx);
        }
    }

    fn on_resume(&mut self, ctx: &mut dyn CueContext) {
        if let Some(m) = self.master.as_deref_mut() {
            m.resume(ctx);
        }
        for i in &mut self.items {
            i.cue.resume(ctx);
        }
    }

    fn media_clock(&self, ctx: &dyn CueContext) -> Option<ShowTime> {
        self.timeline_clock(ctx)
    }

    fn media_finished(&self, ctx: &dyn CueContext) -> bool {
        // With a master but unknown length: done when the master is done.
        match &self.master {
            Some(m) if self.length.is_none() => {
                m.state() == crate::state::CueState::Completed || m.media_finished(ctx)
            }
            _ => false,
        }
    }

    fn validate(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .master
            .iter()
            .flat_map(|m| m.validate())
            .map(|e| format!("master: {e}"))
            .collect();
        for i in &self.items {
            v.extend(i.cue.validate().into_iter().map(|e| format!("{} @ {}: {e}", i.cue.header().number, i.offset)));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{AudioTrack, DmxAction, EngineAction};
    use crate::cue::test_support::*;
    use crate::id::StemId;
    use crate::project::AssetRef;

    fn audio_master() -> AnyCue {
        AnyCue::Audio(AudioCue {
            header: CueHeader::new("10", "Song"),
            tracks: vec![AudioTrack { asset: AssetRef::new("assets/audio/song.wav"), stem: StemId::new(), gain_db: 0.0, mute: false }],
            click: None,
            gain_db: 0.0,
            fade_in: ShowTime::ZERO,
            fade_out: ShowTime::ZERO,
            start_at: ShowTime::ZERO,
            end_at: None,
            looped: false,
            media_length: Some(ShowTime::from_secs(180)),
        })
    }

    fn dmx_scene_count(ctx: &MockCtx) -> usize {
        ctx.actions.iter().filter(|a| matches!(a, EngineAction::Dmx(DmxAction::ApplyScene { .. }))).count()
    }

    #[test]
    fn children_follow_master_media_clock() {
        let mut ctx = MockCtx::default();
        let master = audio_master();
        let master_id = master.header().id;
        let mut tl = TimelineCue::new(CueHeader::new("10", "Song + looks"), Some(master));
        tl.add(ShowTime::from_secs(10), dmx_cue("10.1", 0));
        tl.add(ShowTime::from_secs(30), dmx_cue("10.2", 0));
        let mut tl = AnyCue::Timeline(tl);

        tl.go(&mut ctx);
        assert_eq!(dmx_scene_count(&ctx), 0);

        // The UI clock races ahead but the audio clock says 9.99 s: nothing fires.
        ctx.advance(ShowTime::from_secs(20));
        ctx.positions.insert(master_id, ShowTime::from_millis(9_990));
        tl.tick(&mut ctx);
        assert_eq!(dmx_scene_count(&ctx), 0);

        ctx.positions.insert(master_id, ShowTime::from_millis(10_005));
        tl.tick(&mut ctx);
        assert_eq!(dmx_scene_count(&ctx), 1);
        assert_eq!(tl.elapsed(&ctx), ShowTime::from_millis(10_005));
    }

    #[test]
    fn starting_mid_timeline_chases_state() {
        let mut ctx = MockCtx::default();
        let mut tl = TimelineCue::new(CueHeader::new("1", "Chase"), None);
        tl.add(ShowTime::from_secs(1), dmx_cue("1.1", 0));
        tl.add(ShowTime::from_secs(5), dmx_cue("1.2", 0));
        tl.add(ShowTime::from_secs(60), dmx_cue("1.3", 0));
        let mut tl = AnyCue::Timeline(tl);
        tl.go_from(&mut ctx, ShowTime::from_secs(10));
        // Both past looks are applied so the rig matches the playhead.
        assert_eq!(dmx_scene_count(&ctx), 2);
    }

    #[test]
    fn seeking_a_running_timeline_rechases_children() {
        let mut ctx = MockCtx::default();
        let master = audio_master();
        let master_id = master.header().id;
        let mut tl = TimelineCue::new(CueHeader::new("10", "Song + looks"), Some(master));
        tl.add(ShowTime::from_secs(10), dmx_cue("10.1", 0));
        tl.add(ShowTime::from_secs(30), dmx_cue("10.2", 0));
        let mut tl = AnyCue::Timeline(tl);
        tl.go(&mut ctx);
        ctx.positions.insert(master_id, ShowTime::from_secs(1));
        tl.tick(&mut ctx);
        ctx.actions.clear();

        tl.seek(&mut ctx, ShowTime::from_secs(40));
        // The master is moved in place (no stop / play), both past looks are chased.
        assert!(ctx.actions.iter().any(|a| matches!(a, EngineAction::Audio(crate::action::AudioAction::Seek { cue, position })
            if *cue == master_id && *position == ShowTime::from_secs(40))));
        assert!(!ctx.actions.iter().any(|a| matches!(a, EngineAction::Audio(crate::action::AudioAction::Stop { .. }))));
        assert_eq!(dmx_scene_count(&ctx), 2);
        assert_eq!(tl.state(), crate::state::CueState::Running);
    }
}
