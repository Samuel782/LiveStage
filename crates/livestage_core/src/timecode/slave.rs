//! Timecode slaving: timelines and cue lists that chase the show timecode.
//!
//! * A **timeline** with a [`TimecodeSlave`] runs while timecode is inside
//!   `[start, start + length)`: it starts when timecode rolls into it (or
//!   jumps into the middle — chasing children), follows locates and jumps,
//!   reacts to the stream stopping per [`OnTimecodeStop`], and is re-synced
//!   whenever it drifts more than `tolerance` from timecode.
//! * A **cue list** with [`ListTimecode`] fires every cue that has a
//!   timecode trigger when timecode crosses it, chases on jumps and keeps
//!   triggered cues in sync the same way.
//!
//! Drift between the timecode source and the media clocks is corrected
//! with in-place seeks (audio de-clicks, video re-times) and a hold-off, so
//! a slaved timeline does not thrash while an engine settles after a jump.
//! Cues without a media clock are corrected by re-timing their clock alone,
//! which emits nothing to the engines.

use super::{TcTransport, Timecode};
use crate::cue::{Cue, CueContext, CueExt};
use crate::id::CueId;
use crate::state::CueState;
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// No drift correction this soon after a start / seek / correction.
pub const RESYNC_HOLDOFF: ShowTime = ShowTime::from_millis(250);

/// What a slaved cue does when timecode stops rolling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTimecodeStop {
    /// Pause in place; resume (re-synced) when timecode rolls again.
    #[default]
    Pause,
    /// Stop with the cue's own fade-out.
    Stop,
    /// Keep playing on the local clock.
    Continue,
}

impl OnTimecodeStop {
    pub const ALL: [OnTimecodeStop; 3] = [OnTimecodeStop::Pause, OnTimecodeStop::Stop, OnTimecodeStop::Continue];
    pub fn label(self) -> &'static str {
        match self {
            OnTimecodeStop::Pause => "Pause",
            OnTimecodeStop::Stop => "Stop",
            OnTimecodeStop::Continue => "Keep playing",
        }
    }
}

fn default_tolerance() -> ShowTime {
    ShowTime::from_millis(80)
}

/// Makes a timeline cue follow timecode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimecodeSlave {
    /// Timecode at which the timeline's zero sits.
    pub start: Timecode,
    /// Re-sync when the timeline is further than this from timecode.
    #[serde(default = "default_tolerance")]
    pub tolerance: ShowTime,
    #[serde(default)]
    pub on_stop: OnTimecodeStop,
    #[serde(skip)]
    pub follower: Follower,
}

impl TimecodeSlave {
    pub fn new(start: Timecode) -> Self {
        Self { start, tolerance: default_tolerance(), on_stop: OnTimecodeStop::Pause, follower: Follower::default() }
    }
}

/// Makes a cue list fire its cues' timecode triggers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListTimecode {
    #[serde(default = "default_tolerance")]
    pub tolerance: ShowTime,
    #[serde(default)]
    pub on_stop: OnTimecodeStop,
    #[serde(skip)]
    pub follower: Follower,
    #[serde(skip)]
    pub(crate) holdoff: HashMap<CueId, ShowTime>,
}

impl ListTimecode {
    pub fn new() -> Self {
        Self { tolerance: default_tolerance(), ..Self::default() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Seen {
    running: bool,
    generation: u64,
    position: ShowTime,
}

/// Edge detection on the transport plus follow bookkeeping.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Follower {
    seen: Option<Seen>,
    holdoff: ShowTime,
    /// Drift corrections applied so far (diagnostics).
    pub resyncs: u64,
}

/// Transport changes since the previous tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    pub started: bool,
    pub stopped: bool,
    pub jumped: bool,
    /// Previous position, when timecode was rolling continuously.
    pub prev: Option<ShowTime>,
}

impl Edges {
    fn discontinuity(&self) -> bool {
        self.started || self.jumped
    }
}

impl Follower {
    pub fn observe(&mut self, t: &TcTransport) -> Edges {
        let e = match self.seen {
            // First look: a rolling stream counts as a start (chase in).
            None => Edges { started: t.running, ..Edges::default() },
            Some(s) => Edges {
                started: t.running && !s.running,
                stopped: !t.running && s.running,
                jumped: t.generation != s.generation,
                prev: (s.running && t.running && t.generation == s.generation).then_some(s.position),
            },
        };
        self.seen = Some(Seen { running: t.running, generation: t.generation, position: t.position });
        e
    }
}

/// Signed distance from `start` to `position` (µs), the short way around midnight.
pub fn relative(position: ShowTime, start: Timecode, t: &TcTransport) -> i64 {
    let day = t.rate.day_length().as_micros().max(1) as i64;
    let d = (position.as_micros() as i64 - start.with_rate(t.rate).to_time().as_micros() as i64).rem_euclid(day);
    if d > day / 2 {
        d - day
    } else {
        d
    }
}

/// What following did to a cue this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Followed {
    Nothing,
    Started,
    Resynced,
    Paused,
    Stopped,
}

/// Inputs for [`follow`].
pub(crate) struct Follow<'a> {
    /// Timecode relative to the cue's zero (µs, negative = before it).
    pub rel: i64,
    pub prev_rel: Option<i64>,
    pub edges: Edges,
    pub running: bool,
    pub tolerance: ShowTime,
    pub on_stop: OnTimecodeStop,
    /// Start part-way when timecode jumps or rolls into the cue's window.
    pub chase: bool,
    pub holdoff: &'a mut ShowTime,
}

/// Keeps one cue in step with timecode.
pub(crate) fn follow<C: Cue + ?Sized>(cue: &mut C, ctx: &mut dyn CueContext, f: Follow<'_>) -> Followed {
    let active = cue.state().is_active();
    let len = cue.duration();
    let inside = f.rel >= 0 && len.is_none_or(|l| (f.rel as u64) < l.as_micros());
    let pos = ShowTime::from_micros(f.rel.max(0) as u64);
    let now = ctx.now();

    if !f.running {
        if f.edges.stopped && active {
            match f.on_stop {
                OnTimecodeStop::Pause => {
                    cue.pause(ctx);
                    return Followed::Paused;
                }
                OnTimecodeStop::Stop => {
                    cue.stop(ctx, None);
                    return Followed::Stopped;
                }
                OnTimecodeStop::Continue => {}
            }
        }
        // Located while parked: move the (paused) cue to the new position.
        if f.edges.jumped && active && inside && cue.state() == CueState::Paused {
            cue.seek(ctx, pos);
            return Followed::Resynced;
        }
        return Followed::Nothing;
    }

    // Rolled over the start point during continuous play.
    let crossed = !f.edges.discontinuity() && f.prev_rel.is_some_and(|p| p < 0) && f.rel >= 0;
    if !active {
        if crossed || (f.edges.discontinuity() && inside && f.chase) {
            cue.go_from(ctx, pos);
            *f.holdoff = now + RESYNC_HOLDOFF;
            return Followed::Started;
        }
        return Followed::Nothing;
    }
    if !inside {
        // Jumped outside the window; otherwise let it run out naturally.
        if f.edges.discontinuity() {
            cue.stop(ctx, Some(ShowTime::ZERO));
            return Followed::Stopped;
        }
        return Followed::Nothing;
    }
    if cue.state() == CueState::Paused {
        cue.resume(ctx);
    }
    if f.edges.discontinuity() {
        cue.seek(ctx, pos);
        *f.holdoff = now + RESYNC_HOLDOFF;
        return Followed::Resynced;
    }
    if cue.state() == CueState::Running && now >= *f.holdoff {
        let drift = cue.elapsed(&*ctx).as_micros() as i64 - f.rel;
        if drift.unsigned_abs() > f.tolerance.as_micros() {
            if cue.media_clock(&*ctx).is_some() {
                cue.seek(ctx, pos);
            } else {
                cue.rebase(&*ctx, pos);
            }
            *f.holdoff = now + RESYNC_HOLDOFF;
            return Followed::Resynced;
        }
    }
    Followed::Nothing
}

/// Drives a slaved timeline (any cue, really) from the transport.
pub fn follow_slave<C: Cue + ?Sized>(cue: &mut C, slave: &mut TimecodeSlave, ctx: &mut dyn CueContext, t: &TcTransport) -> Followed {
    let edges = slave.follower.observe(t);
    let rel = relative(t.position, slave.start, t);
    let prev_rel = edges.prev.map(|p| relative(p, slave.start, t));
    let mut holdoff = slave.follower.holdoff;
    let r = follow(
        cue,
        ctx,
        Follow {
            rel,
            prev_rel,
            edges,
            running: t.running,
            tolerance: slave.tolerance,
            on_stop: slave.on_stop,
            chase: true,
            holdoff: &mut holdoff,
        },
    );
    slave.follower.holdoff = holdoff;
    if r == Followed::Resynced && !edges.discontinuity() {
        slave.follower.resyncs += 1;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{AudioAction, AudioTrack, DmxAction, EngineAction};
    use crate::cue::test_support::*;
    use crate::cue::{AnyCue, AudioCue, CueHeader};
    use crate::cuelist::CueList;
    use crate::id::StemId;
    use crate::project::AssetRef;
    use crate::timecode::{FrameRate, LockState};
    use crate::timeline::TimelineCue;

    fn tc(s: &str) -> Timecode {
        s.parse().unwrap()
    }

    fn at(label: &str, plus_ms: u64, running: bool, generation: u64) -> TcTransport {
        TcTransport {
            position: tc(label).to_time() + ShowTime::from_millis(plus_ms),
            rate: FrameRate::Fps25,
            running,
            generation,
            state: if running { LockState::Locked } else { LockState::Stopped },
            speed: 1.0,
        }
    }

    fn song(len_s: u64) -> AnyCue {
        AnyCue::Audio(AudioCue {
            header: CueHeader::new("1.0", "Song"),
            tracks: vec![AudioTrack { asset: AssetRef::new("assets/audio/song.wav"), stem: StemId::new(), gain_db: 0.0, mute: false }],
            click: None,
            gain_db: 0.0,
            fade_in: ShowTime::ZERO,
            fade_out: ShowTime::ZERO,
            start_at: ShowTime::ZERO,
            end_at: None,
            looped: false,
            media_length: Some(ShowTime::from_secs(len_s)),
        })
    }

    /// A 60 s timeline at 01:00:00:00 with DMX looks at +1 s and +30 s.
    fn slaved(master: Option<AnyCue>) -> TimelineCue {
        let mut tl = TimelineCue::new(CueHeader::new("1", "TC show"), master);
        tl.length = Some(ShowTime::from_secs(60));
        tl.add(ShowTime::from_secs(1), dmx_cue("1.1", 0));
        tl.add(ShowTime::from_secs(30), dmx_cue("1.2", 0));
        tl.timecode = Some(TimecodeSlave::new(tc("01:00:00:00")));
        tl
    }

    fn scenes(ctx: &MockCtx) -> usize {
        ctx.actions.iter().filter(|a| matches!(a, EngineAction::Dmx(DmxAction::ApplyScene { .. }))).count()
    }

    #[test]
    fn timeline_starts_when_timecode_rolls_into_it() {
        let mut ctx = MockCtx::default();
        let mut tl = slaved(None);
        tl.follow_timecode(&mut ctx, &at("00:59:59:00", 0, true, 1));
        assert_eq!(tl.state(), CueState::Idle, "before its start");
        ctx.advance(ShowTime::from_millis(1_010));
        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:00:00", 10, true, 1)), Followed::Started);
        assert_eq!(tl.state(), CueState::Running);
        assert_eq!(tl.elapsed(&ctx), ShowTime::from_millis(10), "started exactly as deep as timecode");
        ctx.advance(ShowTime::from_millis(1_000));
        tl.follow_timecode(&mut ctx, &at("01:00:01:00", 10, true, 1));
        tl.tick(&mut ctx);
        assert_eq!(scenes(&ctx), 1, "child fired on the timecode clock");
    }

    #[test]
    fn jump_into_the_middle_chases_children() {
        let mut ctx = MockCtx::default();
        let mut tl = slaved(None);
        tl.follow_timecode(&mut ctx, &at("00:10:00:00", 0, true, 1));
        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:40:00", 0, true, 2)), Followed::Started);
        assert_eq!(tl.elapsed(&ctx), ShowTime::from_secs(40));
        assert_eq!(scenes(&ctx), 2, "both passed looks applied");
        // Jumping out of the window stops it.
        assert_eq!(tl.follow_timecode(&mut ctx, &at("02:00:00:00", 0, true, 3)), Followed::Stopped);
        assert_eq!(tl.state(), CueState::Stopped);
    }

    #[test]
    fn stops_and_restarts_follow_on_stop_policy() {
        let mut ctx = MockCtx::default();
        let mut tl = slaved(Some(song(120)));
        let master = tl.master.as_ref().unwrap().header().id;
        tl.follow_timecode(&mut ctx, &at("01:00:05:00", 0, true, 1));
        assert_eq!(tl.state(), CueState::Running);
        ctx.positions.insert(master, ShowTime::from_secs(5));

        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:05:00", 0, false, 1)), Followed::Paused);
        assert_eq!(tl.state(), CueState::Paused);
        // Sender locates while parked: the paused timeline moves with it.
        ctx.actions.clear();
        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:20:00", 0, false, 2)), Followed::Resynced);
        assert_eq!(tl.state(), CueState::Paused);
        assert!(ctx.actions.iter().any(|a| matches!(a, EngineAction::Audio(AudioAction::Seek { position, .. }) if *position == ShowTime::from_secs(20))));
        // Rolls again: resumed and re-synced.
        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:20:00", 0, true, 3)), Followed::Resynced);
        assert_eq!(tl.state(), CueState::Running);

        let mut stop = slaved(None);
        stop.timecode.as_mut().unwrap().on_stop = OnTimecodeStop::Stop;
        stop.follow_timecode(&mut ctx, &at("01:00:05:00", 0, true, 1));
        stop.follow_timecode(&mut ctx, &at("01:00:05:00", 0, false, 1));
        assert_eq!(stop.state(), CueState::Stopped);

        let mut cont = slaved(None);
        cont.timecode.as_mut().unwrap().on_stop = OnTimecodeStop::Continue;
        cont.follow_timecode(&mut ctx, &at("01:00:05:00", 0, true, 1));
        cont.follow_timecode(&mut ctx, &at("01:00:05:00", 0, false, 1));
        assert_eq!(cont.state(), CueState::Running);
    }

    #[test]
    fn media_drift_beyond_tolerance_is_resynced_with_holdoff() {
        let mut ctx = MockCtx::default();
        let mut tl = slaved(Some(song(120)));
        let master = tl.master.as_ref().unwrap().header().id;
        tl.follow_timecode(&mut ctx, &at("01:00:10:00", 0, true, 1));
        ctx.advance(ShowTime::from_secs(1));
        // The audio device lags timecode by 50 ms: inside the 80 ms tolerance.
        ctx.positions.insert(master, ShowTime::from_millis(10_950));
        ctx.actions.clear();
        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:11:00", 0, true, 1)), Followed::Nothing);
        // 120 ms behind: re-synced in place with an audio seek.
        ctx.advance(ShowTime::from_secs(1));
        ctx.positions.insert(master, ShowTime::from_millis(11_880));
        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:12:00", 0, true, 1)), Followed::Resynced);
        assert!(ctx.actions.iter().any(|a| matches!(a, EngineAction::Audio(AudioAction::Seek { cue, position }) if *cue == master && *position == ShowTime::from_secs(12))));
        assert!(!ctx.actions.iter().any(|a| matches!(a, EngineAction::Audio(AudioAction::Stop { .. }))), "no restart");
        // Still off right after (engine settling): the hold-off prevents thrashing.
        ctx.advance(ShowTime::from_millis(100));
        ctx.positions.insert(master, ShowTime::from_millis(11_900));
        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:12:00", 100, true, 1)), Followed::Nothing);
        assert_eq!(tl.timecode.as_ref().unwrap().follower.resyncs, 1);
    }

    #[test]
    fn clock_only_timelines_are_corrected_silently() {
        let mut ctx = MockCtx::default();
        let mut tl = slaved(None);
        tl.follow_timecode(&mut ctx, &at("01:00:02:00", 0, true, 1));
        tl.tick(&mut ctx);
        // Local clock ran 10 s, timecode 10.2 s (sender 2 % fast).
        ctx.advance(ShowTime::from_secs(10));
        ctx.actions.clear();
        assert_eq!(tl.follow_timecode(&mut ctx, &at("01:00:12:00", 200, true, 1)), Followed::Resynced);
        assert!(ctx.actions.is_empty(), "no re-trigger of looks or media");
        assert_eq!(tl.elapsed(&ctx), ShowTime::from_millis(12_200));
    }

    #[test]
    fn cue_list_fires_timecode_triggers_and_chases() {
        let mut ctx = MockCtx::default();
        let mut l = CueList::new("TC");
        for (n, t) in [("1", "01:00:00:00"), ("2", "01:00:10:00"), ("3", "01:00:20:00")] {
            let mut c = dmx_cue(n, 5_000);
            c.header_mut().timecode = Some(tc(t));
            l.cues.push(c);
        }
        l.cues.push(dmx_cue("4", 0)); // manual cue: untouched by timecode
        l.timecode = Some(ListTimecode::new());
        l.reset(&mut ctx);

        l.follow_timecode(&mut ctx, &at("00:59:59:20", 0, true, 1));
        assert!(l.cues.iter().all(|c| c.state() != CueState::Running));
        ctx.advance(ShowTime::from_millis(200));
        l.follow_timecode(&mut ctx, &at("01:00:00:00", 5, true, 1));
        assert_eq!(l.cues[0].state(), CueState::Running);
        assert_eq!(l.standby_index(), Some(1), "playhead moves to the next trigger");

        // Jump to 01:00:22: cue 3's fade window covers it and is chased,
        // cue 1 and 2 (fades over) are not restarted.
        l.cues[0].stop(&mut ctx, Some(ShowTime::ZERO));
        l.follow_timecode(&mut ctx, &at("01:00:22:00", 0, true, 2));
        assert_eq!(l.cues[2].state(), CueState::Running);
        assert_eq!(l.cues[2].elapsed(&ctx), ShowTime::from_secs(2));
        assert_ne!(l.cues[1].state(), CueState::Running);
        assert!(!l.cues[3].state().is_active(), "manual cue not fired");

        // Past the last trigger the playhead moves on like GO (to manual cue 4).
        assert_eq!(l.standby_index(), Some(3));

        // Timecode stops: triggered cues pause.
        l.follow_timecode(&mut ctx, &at("01:00:22:00", 0, false, 2));
        assert_eq!(l.cues[2].state(), CueState::Paused);
    }

    #[test]
    fn slave_config_serialises() {
        let mut tl = slaved(None);
        tl.timecode.as_mut().unwrap().on_stop = OnTimecodeStop::Continue;
        let json = serde_json::to_string(&AnyCue::Timeline(tl.clone())).unwrap();
        assert!(json.contains("\"start\":\"01:00:00:00@25\""), "{json}");
        let back: AnyCue = serde_json::from_str(&json).unwrap();
        let AnyCue::Timeline(b) = back else { panic!() };
        assert_eq!(b.timecode.as_ref().unwrap().on_stop, OnTimecodeStop::Continue);
        // Older shows without timecode fields still load.
        let plain = serde_json::to_string(&AnyCue::Timeline(TimelineCue::new(CueHeader::new("2", "x"), None))).unwrap();
        assert!(!plain.contains("timecode"));
    }
}
