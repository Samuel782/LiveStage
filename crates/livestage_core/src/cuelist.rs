//! Cue lists and their execution state machine.
//!
//! ```text
//!            set_standby            GO                    tick (pre-wait over)
//!   Idle ──────────────▶ Standby ───────▶ PreWait ───────────────▶ Running
//!                                                                 │  │  ▲
//!                                     duration / media end ◀──────┘  │  │ resume
//!                                          Completed          pause ▼  │
//!                                                                  Paused
//!   any active ── stop / panic ──▶ Stopped
//! ```
//!
//! After GO the standby pointer advances to the next *armed* cue.
//! `AutoContinue` fires the next cue `post_wait` after this one starts;
//! `AutoFollow` fires it `post_wait` after this one completes.

use crate::action::{AudioAction, DmxAction, EngineAction, VideoAction};
use crate::cue::{AnyCue, Cue, CueContext, CueEvent, CueExt, FollowMode};
use crate::id::{CueId, CueListId};
use crate::state::CueState;
use crate::time::ShowTime;
use crate::timecode::slave::{follow, relative, Follow, Followed};
use crate::timecode::{ListTimecode, TcTransport};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq)]
struct Pending {
    index: usize,
    at: ShowTime,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueListRuntime {
    standby: Option<usize>,
    pending: Vec<Pending>,
    last_go: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueList {
    pub id: CueListId,
    pub name: String,
    pub cues: Vec<AnyCue>,
    /// Timecode mode: cues with a timecode trigger fire on their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timecode: Option<ListTimecode>,
    #[serde(skip)]
    runtime: CueListRuntime,
}

/// Serialisable status, used by the UI and the remote API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueStatus {
    pub id: CueId,
    pub number: String,
    pub name: String,
    pub state: CueState,
    pub elapsed: ShowTime,
    pub remaining: Option<ShowTime>,
    pub progress: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueListStatus {
    pub id: CueListId,
    pub name: String,
    pub standby: Option<CueStatus>,
    pub active: Vec<CueStatus>,
}

impl CueList {
    pub fn new(name: impl Into<String>) -> Self {
        Self { id: CueListId::new(), name: name.into(), cues: Vec::new(), timecode: None, runtime: CueListRuntime::default() }
    }

    // ------------------------------------------------------------ queries --

    pub fn standby_index(&self) -> Option<usize> {
        self.runtime.standby
    }
    pub fn standby_cue(&self) -> Option<&AnyCue> {
        self.runtime.standby.and_then(|i| self.cues.get(i))
    }
    pub fn last_go_index(&self) -> Option<usize> {
        self.runtime.last_go
    }
    pub fn index_of(&self, id: CueId) -> Option<usize> {
        self.cues.iter().position(|c| c.header().id == id)
    }
    pub fn index_of_number(&self, number: &str) -> Option<usize> {
        self.cues.iter().position(|c| c.header().number == number)
    }
    pub fn find(&self, id: CueId) -> Option<&AnyCue> {
        self.cues.iter().find_map(|c| c.find(id))
    }
    pub fn is_running(&self) -> bool {
        self.cues.iter().any(|c| c.state().is_active()) || !self.runtime.pending.is_empty()
    }

    fn next_armed(&self, after: Option<usize>) -> Option<usize> {
        let start = after.map_or(0, |i| i + 1);
        (start..self.cues.len()).find(|&i| self.cues[i].header().armed)
    }
    fn prev_armed(&self, before: usize) -> Option<usize> {
        (0..before).rev().find(|&i| self.cues[i].header().armed)
    }

    // ------------------------------------------------------------ standby --

    /// Puts cue `index` under the playhead (and preloads it).
    pub fn set_standby(&mut self, ctx: &mut dyn CueContext, index: Option<usize>) {
        if let Some(old) = self.runtime.standby.and_then(|i| self.cues.get_mut(i)) {
            old.unstandby();
        }
        self.runtime.standby = index.filter(|&i| i < self.cues.len());
        if let Some(cue) = self.runtime.standby.and_then(|i| self.cues.get_mut(i)) {
            cue.standby(ctx);
        }
    }

    /// Standby the first armed cue (call after loading a show).
    pub fn reset(&mut self, ctx: &mut dyn CueContext) {
        for c in &mut self.cues {
            c.reset();
        }
        self.runtime = CueListRuntime::default();
        let first = self.next_armed(None);
        self.set_standby(ctx, first);
    }

    pub fn standby_next(&mut self, ctx: &mut dyn CueContext) {
        let next = self.next_armed(self.runtime.standby).or(self.runtime.standby);
        self.set_standby(ctx, next);
    }

    pub fn standby_previous(&mut self, ctx: &mut dyn CueContext) {
        let cur = self.runtime.standby.unwrap_or(self.cues.len());
        let prev = self.prev_armed(cur).or(self.runtime.standby);
        self.set_standby(ctx, prev);
    }

    // ----------------------------------------------------------------- GO --

    /// Fires the standby cue and advances the playhead. Returns the fired cue.
    pub fn go(&mut self, ctx: &mut dyn CueContext) -> Option<CueId> {
        let index = self.runtime.standby?;
        let id = self.fire(ctx, index);
        let next = self.next_armed(Some(index));
        self.set_standby(ctx, next);
        Some(id)
    }

    /// Fires a specific cue by number and moves the playhead after it.
    pub fn go_number(&mut self, ctx: &mut dyn CueContext, number: &str) -> Option<CueId> {
        let index = self.index_of_number(number)?;
        self.set_standby(ctx, Some(index));
        self.go(ctx)
    }

    fn fire(&mut self, ctx: &mut dyn CueContext, index: usize) -> CueId {
        self.cues[index].go(ctx);
        let pre_wait = self.cues[index].header().pre_wait;
        self.after_fire(ctx, index, pre_wait, ShowTime::ZERO);
        self.cues[index].header().id
    }

    /// Bookkeeping after cue `index` was started (`start_in` from now,
    /// already `into` deep): playhead history and auto-continue.
    fn after_fire(&mut self, ctx: &mut dyn CueContext, index: usize, start_in: ShowTime, into: ShowTime) {
        let h = self.cues[index].header();
        let (follow, post_wait) = (h.follow, h.post_wait);
        self.runtime.last_go = Some(index);
        if follow == FollowMode::AutoContinue {
            if let Some(next) = self.next_armed(Some(index)) {
                // Continue is measured from when this cue actually starts.
                self.runtime.pending.push(Pending { index: next, at: ctx.now() + start_in + (post_wait - into) });
            }
        }
    }

    // ----------------------------------------------------------- timecode --

    /// Follows show timecode: slaved timelines chase it, and in timecode
    /// mode every armed cue with a timecode trigger fires when timecode
    /// reaches it (and is chased, re-synced and paused like a timeline).
    /// Call once per show tick, before [`CueList::tick`].
    pub fn follow_timecode(&mut self, ctx: &mut dyn CueContext, t: &TcTransport) {
        for c in &mut self.cues {
            if let AnyCue::Timeline(tl) = c {
                tl.follow_timecode(ctx, t);
            }
        }
        let Some(mut tc) = self.timecode.take() else { return };
        let edges = tc.follower.observe(t);
        let mut last_fired = None;
        for i in 0..self.cues.len() {
            let cue = &self.cues[i];
            let Some(trigger) = cue.header().timecode else { continue };
            // Self-slaved timelines were handled above.
            if !cue.header().armed || matches!(cue, AnyCue::Timeline(tl) if tl.timecode.is_some()) {
                continue;
            }
            let id = cue.header().id;
            let rel = relative(t.position, trigger, t);
            let mut holdoff = tc.holdoff.get(&id).copied().unwrap_or_default();
            let chase = cue.chase_on_catch_up();
            let r = follow(
                &mut self.cues[i],
                ctx,
                Follow {
                    rel,
                    prev_rel: edges.prev.map(|p| relative(p, trigger, t)),
                    edges,
                    running: t.running,
                    tolerance: tc.tolerance,
                    on_stop: tc.on_stop,
                    chase,
                    holdoff: &mut holdoff,
                },
            );
            tc.holdoff.insert(id, holdoff);
            if r == Followed::Started {
                self.after_fire(ctx, i, ShowTime::ZERO, ShowTime::from_micros(rel.max(0) as u64));
                last_fired = Some(i);
            } else if r == Followed::Resynced && !edges.started && !edges.jumped {
                tc.follower.resyncs += 1;
            }
        }
        // Playhead: standby the next cue timecode will reach, or (past the
        // last trigger) the cue after the one just fired, as GO would.
        if last_fired.is_some() || edges.started || edges.jumped {
            let next = (0..self.cues.len())
                .find(|&i| {
                    let h = self.cues[i].header();
                    h.armed && h.timecode.is_some_and(|tr| relative(t.position, tr, t) < 0)
                })
                .or_else(|| last_fired.and_then(|i| self.next_armed(Some(i))));
            if next.is_some() && next != self.runtime.standby {
                self.set_standby(ctx, next);
            }
        }
        self.timecode = Some(tc);
    }

    // --------------------------------------------------------------- tick --

    /// Advances every active cue and processes follow-ons. Call once per
    /// control frame (typically 100–250 Hz on the show thread).
    pub fn tick(&mut self, ctx: &mut dyn CueContext) {
        for i in 0..self.cues.len() {
            if !self.cues[i].state().is_active() {
                continue;
            }
            if self.cues[i].tick(ctx) == Some(CueEvent::Completed) {
                let h = self.cues[i].header();
                if h.follow == FollowMode::AutoFollow {
                    if let Some(next) = self.next_armed(Some(i)) {
                        self.runtime.pending.push(Pending { index: next, at: ctx.now() + h.post_wait });
                    }
                }
            }
        }

        let now = ctx.now();
        let mut due: Vec<usize> = Vec::new();
        self.runtime.pending.retain(|p| {
            if p.at <= now {
                due.push(p.index);
                false
            } else {
                true
            }
        });
        for index in due {
            self.fire(ctx, index);
            // If the chain fired the standby cue, move the playhead past it.
            if self.runtime.standby == Some(index) {
                let next = self.next_armed(Some(index));
                self.set_standby(ctx, next);
            }
        }
    }

    // ------------------------------------------------------- stop / panic --

    pub fn pause_all(&mut self, ctx: &mut dyn CueContext) {
        for c in &mut self.cues {
            c.pause(ctx);
        }
    }

    pub fn resume_all(&mut self, ctx: &mut dyn CueContext) {
        for c in &mut self.cues {
            c.resume(ctx);
        }
    }

    pub fn stop_cue(&mut self, ctx: &mut dyn CueContext, id: CueId, fade: Option<ShowTime>) {
        if let Some(i) = self.index_of(id) {
            self.cues[i].stop(ctx, fade);
        }
    }

    /// Scrubs a cue (top level or timeline child) to `position`.
    pub fn seek_cue(&mut self, ctx: &mut dyn CueContext, id: CueId, position: ShowTime) -> bool {
        fn find(c: &mut AnyCue, id: CueId) -> Option<&mut AnyCue> {
            if c.header().id == id {
                return Some(c);
            }
            if let AnyCue::Timeline(t) = c {
                if let Some(m) = t.master.as_deref_mut().and_then(|m| find(m, id)) {
                    return Some(m);
                }
                return t.items.iter_mut().find_map(|i| find(&mut i.cue, id));
            }
            None
        }
        match self.cues.iter_mut().find_map(|c| find(c, id)) {
            Some(c) => {
                c.seek(ctx, position);
                true
            }
            None => false,
        }
    }

    pub fn stop_all(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        self.runtime.pending.clear();
        for c in &mut self.cues {
            c.stop(ctx, fade);
        }
    }

    /// Panic: cancel all follow-ons, stop every cue, and tell every engine to
    /// silence itself. Soft panic fades quickly; hard panic cuts instantly.
    pub fn panic(&mut self, ctx: &mut dyn CueContext, hard: bool) {
        let fade = if hard { ShowTime::ZERO } else { ShowTime::from_millis(1_500) };
        self.stop_all(ctx, Some(fade));
        ctx.dispatch(EngineAction::Audio(AudioAction::StopAll { fade }));
        ctx.dispatch(EngineAction::Video(VideoAction::BlackoutAll { fade }));
        if hard {
            ctx.dispatch(EngineAction::Dmx(DmxAction::Blackout { fade }));
        }
        tracing::warn!(hard, list = %self.name, "PANIC");
    }

    // ----------------------------------------------------- undo support --

    /// Copies live execution state from `old` (same list before an edit was
    /// undone / redone) so running cues stay controllable. Matching is by cue
    /// id, so reordered, added and removed cues are handled.
    pub fn adopt_runtime_from(&mut self, old: &CueList) {
        for c in &mut self.cues {
            adopt_cue(c, old);
        }
        if let (Some(tc), Some(otc)) = (&mut self.timecode, &old.timecode) {
            tc.follower = otc.follower.clone();
            tc.holdoff = otc.holdoff.clone();
        }
        let remap = |i: usize| old.cues.get(i).map(|c| c.header().id).and_then(|id| self.index_of(id));
        self.runtime = CueListRuntime {
            standby: old.runtime.standby.and_then(remap),
            pending: old
                .runtime
                .pending
                .iter()
                .filter_map(|p| remap(p.index).map(|index| Pending { index, at: p.at }))
                .collect(),
            last_go: old.runtime.last_go.and_then(remap),
        };
    }

    // ------------------------------------------------------------- status --

    pub fn status(&self, ctx: &dyn CueContext) -> CueListStatus {
        let st = |c: &AnyCue| CueStatus {
            id: c.header().id,
            number: c.header().number.clone(),
            name: c.header().name.clone(),
            state: c.state(),
            elapsed: c.elapsed(ctx),
            remaining: c.remaining(ctx),
            progress: c.progress(ctx),
        };
        CueListStatus {
            id: self.id,
            name: self.name.clone(),
            standby: self.standby_cue().map(st),
            active: self.cues.iter().filter(|c| c.state().is_active()).map(st).collect(),
        }
    }
}

fn adopt_cue(new: &mut AnyCue, old_list: &CueList) {
    let id = new.header().id;
    let Some(old) = old_list.find(id) else { return };
    new.header_mut().runtime = old.header().runtime.clone();
    if let (AnyCue::Slides(s), AnyCue::Slides(os)) = (&mut *new, old) {
        // Keep the slide on screen when an edit swaps the project.
        s.current = os.current.min(s.slide_count().saturating_sub(1));
    }
    if let (AnyCue::Timeline(t), AnyCue::Timeline(ot)) = (new, old) {
        if let (Some(tc), Some(otc)) = (&mut t.timecode, &ot.timecode) {
            tc.follower = otc.follower.clone();
        }
        if let Some(m) = t.master.as_deref_mut() {
            adopt_cue(m, old_list);
        }
        for item in &mut t.items {
            let iid = item.cue.header().id;
            if let Some(oi) = ot.items.iter().find(|i| i.cue.header().id == iid) {
                item.fired = oi.fired;
            }
            adopt_cue(&mut item.cue, old_list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::test_support::*;

    fn list(n: usize) -> CueList {
        let mut l = CueList::new("Main");
        for i in 1..=n {
            l.cues.push(dmx_cue(&i.to_string(), 1_000));
        }
        l
    }

    #[test]
    fn go_advances_standby_and_skips_disarmed() {
        let mut ctx = MockCtx::default();
        let mut l = list(4);
        l.cues[1].header_mut().armed = false;
        l.reset(&mut ctx);
        assert_eq!(l.standby_index(), Some(0));
        assert_eq!(l.cues[0].state(), CueState::Standby);
        l.go(&mut ctx);
        assert_eq!(l.standby_index(), Some(2));
        assert_eq!(l.cues[0].state(), CueState::Running);
        assert_eq!(l.cues[1].state(), CueState::Idle);
    }

    #[test]
    fn auto_continue_and_auto_follow() {
        let mut ctx = MockCtx::default();
        let mut l = list(3);
        l.cues[0].header_mut().follow = FollowMode::AutoContinue;
        l.cues[0].header_mut().post_wait = ShowTime::from_millis(200);
        l.cues[1].header_mut().follow = FollowMode::AutoFollow;
        l.reset(&mut ctx);

        l.go(&mut ctx);
        assert_eq!(l.cues[1].state(), CueState::Standby);
        ctx.advance(ShowTime::from_millis(200));
        l.tick(&mut ctx);
        assert_eq!(l.cues[1].state(), CueState::Running, "auto-continue fired cue 2");
        assert_eq!(l.standby_index(), Some(2));

        ctx.advance(ShowTime::from_millis(1_000));
        l.tick(&mut ctx); // cue 2 completes -> schedules cue 3 now
        l.tick(&mut ctx);
        assert_eq!(l.cues[2].state(), CueState::Running, "auto-follow fired cue 3");
        assert_eq!(l.standby_index(), None);
    }

    #[test]
    fn undo_snapshot_keeps_running_state() {
        let mut ctx = MockCtx::default();
        let mut l = list(3);
        l.reset(&mut ctx);
        let snapshot = l.clone(); // state before GO (as an undo stack would hold)
        l.go(&mut ctx);
        // Restore the snapshot with a cue inserted at the top and adopt live state.
        let mut restored = snapshot.clone();
        restored.cues.insert(0, dmx_cue("0", 0));
        restored.adopt_runtime_from(&l);
        assert_eq!(restored.cues[1].state(), CueState::Running, "running cue survives undo");
        assert_eq!(restored.standby_index(), Some(2), "standby follows the cue, not the index");
        restored.stop_all(&mut ctx, None);
        assert_eq!(restored.cues[1].state(), CueState::Stopped);
    }

    #[test]
    fn panic_stops_everything_and_cancels_follows() {
        let mut ctx = MockCtx::default();
        let mut l = list(2);
        l.cues[0].header_mut().follow = FollowMode::AutoContinue;
        l.cues[0].header_mut().post_wait = ShowTime::from_secs(5);
        l.reset(&mut ctx);
        l.go(&mut ctx);
        l.panic(&mut ctx, true);
        assert!(!l.is_running());
        assert_eq!(l.cues[0].state(), CueState::Stopped);
        ctx.advance(ShowTime::from_secs(10));
        l.tick(&mut ctx);
        assert_ne!(l.cues[1].state(), CueState::Running);
        assert!(ctx.actions.iter().any(|a| matches!(a, EngineAction::Dmx(DmxAction::Blackout { .. }))));
    }
}
