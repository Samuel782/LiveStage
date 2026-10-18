//! The [`Cue`] trait, concrete cue types and the shared lifecycle.
//!
//! ## Design
//!
//! * **Data + hooks.** Each concrete cue (e.g. [`AudioCue`]) is plain
//!   serialisable data plus a handful of hooks (`on_go`, `on_stop`, ...).
//! * **One state machine.** The lifecycle (pre-wait → running → completed /
//!   stopped, pause/resume, elapsed-time accounting, media-clock slaving) is
//!   implemented once in [`CueExt`], blanket-implemented for every `Cue`,
//!   including `dyn Cue`.
//! * **No hardware.** Hooks emit [`EngineAction`]s via [`CueContext`].
//! * **Serialisation.** Trait objects don't serialise, so the show tree
//!   stores [`AnyCue`], a tagged enum that implements `Cue` by delegation.

use crate::action::*;
use crate::id::{CueId, OutputId};
use crate::state::{CueState, Rgb};
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The bridge between cues and the running engines.
pub trait CueContext {
    /// Monotonic show clock (wall clock while the show is open).
    fn now(&self) -> ShowTime;
    /// Sends an action to an engine. Must not block.
    fn dispatch(&mut self, action: EngineAction);
    /// Engine-reported elapsed playback time of a cue's media, if the cue
    /// is clocked by an engine (audio sample clock, video PTS).
    fn media_position(&self, _cue: CueId) -> Option<ShowTime> {
        None
    }
    /// Whether the engine reports the cue's media as having reached its end.
    fn media_finished(&self, _cue: CueId) -> bool {
        false
    }
}

/// What happens after a cue is fired.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FollowMode {
    /// Wait for the operator.
    #[default]
    None,
    /// Fire the next cue `post_wait` after this cue *starts*.
    AutoContinue,
    /// Fire the next cue `post_wait` after this cue *completes*.
    AutoFollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CueKind {
    Audio,
    Video,
    Timer,
    Dmx,
    Control,
    Timeline,
    Slides,
}

impl CueKind {
    pub fn label(self) -> &'static str {
        match self {
            CueKind::Audio => "Audio",
            CueKind::Video => "Video",
            CueKind::Timer => "Timer",
            CueKind::Dmx => "DMX",
            CueKind::Control => "Control",
            CueKind::Timeline => "Timeline",
            CueKind::Slides => "Slides",
        }
    }
}

/// Non-persistent execution bookkeeping.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueRuntime {
    pub state: CueState,
    /// When GO was received.
    pub go_at: ShowTime,
    /// When the cue actually started (after pre-wait).
    pub started_at: ShowTime,
    /// Position the cue was started from (seek / timeline catch-up).
    pub start_offset: ShowTime,
    pub paused_at: Option<ShowTime>,
    pub paused_total: ShowTime,
    /// Last computed elapsed time (for display after stop/complete).
    pub last_elapsed: ShowTime,
}

/// Fields shared by every cue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueHeader {
    pub id: CueId,
    /// Operator-facing cue number, e.g. `"1"`, `"12.5"`, `"A3"`.
    pub number: String,
    pub name: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pre_wait: ShowTime,
    #[serde(default)]
    pub post_wait: ShowTime,
    #[serde(default)]
    pub follow: FollowMode,
    /// Disarmed cues are skipped by GO.
    #[serde(default = "yes")]
    pub armed: bool,
    #[serde(default)]
    pub color: Option<Rgb>,
    /// Fire when show timecode reaches this (cue lists in timecode mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timecode: Option<crate::timecode::Timecode>,
    #[serde(skip)]
    pub runtime: CueRuntime,
}
fn yes() -> bool {
    true
}

impl CueHeader {
    pub fn new(number: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: CueId::new(),
            number: number.into(),
            name: name.into(),
            notes: String::new(),
            pre_wait: ShowTime::ZERO,
            post_wait: ShowTime::ZERO,
            follow: FollowMode::None,
            armed: true,
            color: None,
            timecode: None,
            runtime: CueRuntime::default(),
        }
    }
}

/// Lifecycle notifications returned by [`CueExt::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueEvent {
    Started,
    Completed,
}

/// A cue: something that can be fired and that runs over (possibly zero) time.
///
/// Implementors provide data access and hooks; the lifecycle lives in
/// [`CueExt`]. Hooks must be cheap and non-blocking — anything heavy is
/// delegated to engines through [`CueContext::dispatch`].
pub trait Cue: Send + fmt::Debug {
    fn header(&self) -> &CueHeader;
    fn header_mut(&mut self) -> &mut CueHeader;
    fn kind(&self) -> CueKind;

    /// Nominal running time; `None` = until stopped (loops, free timers) or
    /// until the engine reports the media finished.
    fn duration(&self) -> Option<ShowTime>;

    /// Called when the cue goes to standby: preload assets.
    fn prepare(&mut self, _ctx: &mut dyn CueContext) {}

    /// Start running from `offset` into the cue.
    fn on_go(&mut self, ctx: &mut dyn CueContext, offset: ShowTime);

    /// Stop running. `fade = None` means "use the cue's own fade-out".
    fn on_stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>);

    fn on_pause(&mut self, _ctx: &mut dyn CueContext) {}
    fn on_resume(&mut self, _ctx: &mut dyn CueContext) {}

    /// Jump a running or paused cue to `offset` without restarting it
    /// (live scrubbing). Return `false` to fall back to stop + restart.
    fn on_seek(&mut self, _ctx: &mut dyn CueContext, _offset: ShowTime) -> bool {
        false
    }

    /// Per-frame update while running.
    fn on_tick(&mut self, _ctx: &mut dyn CueContext, _elapsed: ShowTime) {}

    /// Natural completion (duration reached / media ended).
    fn on_complete(&mut self, _ctx: &mut dyn CueContext) {}

    /// Authoritative clock for this cue, if an engine provides one.
    fn media_clock(&self, _ctx: &dyn CueContext) -> Option<ShowTime> {
        None
    }

    /// Engine-driven completion (e.g. an audio file without known length).
    fn media_finished(&self, _ctx: &dyn CueContext) -> bool {
        false
    }

    /// Whether to fire this cue when a timeline is started *past* its offset.
    /// Triggers (OSC/MIDI) usually shouldn't re-fire; media and DMX should chase.
    fn chase_on_catch_up(&self) -> bool {
        true
    }

    /// Human-readable configuration problems.
    fn validate(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Lifecycle state machine shared by every cue.
pub trait CueExt: Cue {
    fn id(&self) -> CueId {
        self.header().id
    }
    fn state(&self) -> CueState {
        self.header().runtime.state
    }

    /// Mark as standby and preload.
    fn standby(&mut self, ctx: &mut dyn CueContext) {
        if !self.state().is_active() {
            self.header_mut().runtime.state = CueState::Standby;
        }
        self.prepare(ctx);
    }

    /// Leave standby without running.
    fn unstandby(&mut self) {
        if self.state() == CueState::Standby {
            self.header_mut().runtime.state = CueState::Idle;
        }
    }

    /// GO: honours pre-wait.
    fn go(&mut self, ctx: &mut dyn CueContext) {
        if self.state().is_active() {
            // Re-trigger: restart from the top.
            self.stop(ctx, Some(ShowTime::ZERO));
        }
        let now = ctx.now();
        let pre_wait = self.header().pre_wait;
        let rt = &mut self.header_mut().runtime;
        *rt = CueRuntime { go_at: now, ..CueRuntime::default() };
        if pre_wait.is_zero() {
            start(self, ctx, ShowTime::ZERO);
        } else {
            self.header_mut().runtime.state = CueState::PreWait;
        }
    }

    /// Start immediately at `offset`, skipping pre-wait (timeline chase, seek).
    fn go_from(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) {
        if self.state().is_active() {
            self.stop(ctx, Some(ShowTime::ZERO));
        }
        let now = ctx.now();
        self.header_mut().runtime = CueRuntime { go_at: now, ..CueRuntime::default() };
        start(self, ctx, offset);
    }

    /// Advances the cue; returns lifecycle transitions that happened.
    fn tick(&mut self, ctx: &mut dyn CueContext) -> Option<CueEvent> {
        match self.state() {
            CueState::PreWait => {
                let rt = &self.header().runtime;
                if ctx.now() >= rt.go_at + self.header().pre_wait {
                    start(self, ctx, ShowTime::ZERO);
                    Some(CueEvent::Started)
                } else {
                    None
                }
            }
            CueState::Running => {
                let elapsed = self.elapsed(&*ctx);
                self.header_mut().runtime.last_elapsed = elapsed;
                self.on_tick(ctx, elapsed);
                let done = self.duration().is_some_and(|d| elapsed >= d) || self.media_finished(&*ctx);
                if done {
                    self.header_mut().runtime.state = CueState::Completed;
                    self.on_complete(ctx);
                    Some(CueEvent::Completed)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        let state = self.state();
        if state.is_active() || state == CueState::Completed {
            if state != CueState::PreWait {
                let elapsed = self.elapsed(&*ctx);
                self.header_mut().runtime.last_elapsed = elapsed;
                self.on_stop(ctx, fade);
            }
            self.header_mut().runtime.state = CueState::Stopped;
        }
    }

    fn pause(&mut self, ctx: &mut dyn CueContext) {
        if self.state() == CueState::Running {
            let now = ctx.now();
            self.header_mut().runtime.paused_at = Some(now);
            self.header_mut().runtime.state = CueState::Paused;
            self.on_pause(ctx);
        }
    }

    fn resume(&mut self, ctx: &mut dyn CueContext) {
        if self.state() == CueState::Paused {
            let now = ctx.now();
            let rt = &mut self.header_mut().runtime;
            if let Some(p) = rt.paused_at.take() {
                rt.paused_total += now - p;
            }
            rt.state = CueState::Running;
            self.on_resume(ctx);
        }
    }

    /// Jump to `position` and keep running (or stay paused).
    ///
    /// A running cue is moved in place: engines jump their playheads (audio
    /// with a de-click dip, video by re-timing the layer) and nothing is
    /// stopped or reloaded. Cues that cannot seek in place are restarted at
    /// `position`. A cue that is not running is started at `position`.
    fn seek(&mut self, ctx: &mut dyn CueContext, position: ShowTime) {
        let position = match self.duration() {
            // Stay just inside the cue so seeking to the end does not complete it.
            Some(d) if !d.is_zero() => position.min(d - ShowTime::from_millis(1)),
            _ => position,
        }
        .max(ShowTime::ZERO);
        let state = self.state();
        let was_paused = state == CueState::Paused;
        if matches!(state, CueState::Running | CueState::Paused) && self.on_seek(ctx, position) {
            let now = ctx.now();
            let rt = &mut self.header_mut().runtime;
            rt.started_at = now;
            rt.start_offset = position;
            rt.paused_total = ShowTime::ZERO;
            rt.paused_at = was_paused.then_some(now);
            rt.last_elapsed = position;
            return;
        }
        self.go_from(ctx, position);
        if was_paused {
            self.pause(ctx);
        }
    }

    /// Re-times the cue's own clock so `elapsed` reads `position`, without
    /// telling any engine. Used for sub-frame drift correction of cues whose
    /// clock is the show clock (timecode slaving); media cues seek instead.
    fn rebase(&mut self, ctx: &dyn CueContext, position: ShowTime) {
        if self.state() != CueState::Running {
            return;
        }
        let now = ctx.now();
        let rt = &mut self.header_mut().runtime;
        rt.started_at = now;
        rt.start_offset = position;
        rt.paused_total = ShowTime::ZERO;
        rt.last_elapsed = position;
    }

    /// Return to idle (e.g. after a stop, when the list is reset).
    fn reset(&mut self) {
        self.header_mut().runtime = CueRuntime::default();
    }

    /// Elapsed running time. Slaved to the media clock when one exists so
    /// child cues follow the audio sample clock rather than the UI clock.
    fn elapsed(&self, ctx: &dyn CueContext) -> ShowTime {
        let rt = &self.header().runtime;
        match rt.state {
            CueState::Running | CueState::Paused => {
                if let Some(t) = self.media_clock(ctx) {
                    return t;
                }
                let now = rt.paused_at.unwrap_or_else(|| ctx.now());
                (now - rt.started_at - rt.paused_total) + rt.start_offset
            }
            CueState::PreWait => ShowTime::ZERO,
            _ => rt.last_elapsed,
        }
    }

    fn remaining(&self, ctx: &dyn CueContext) -> Option<ShowTime> {
        self.duration().map(|d| d - self.elapsed(ctx))
    }

    /// 0..1 progress for progress bars (None for indefinite cues).
    fn progress(&self, ctx: &dyn CueContext) -> Option<f32> {
        let d = self.duration()?;
        if d.is_zero() {
            return Some(1.0);
        }
        Some((self.elapsed(ctx).as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0) as f32)
    }
}

impl<T: Cue + ?Sized> CueExt for T {}

fn start<C: Cue + ?Sized>(cue: &mut C, ctx: &mut dyn CueContext, offset: ShowTime) {
    let now = ctx.now();
    {
        let rt = &mut cue.header_mut().runtime;
        rt.state = CueState::Running;
        rt.started_at = now;
        rt.start_offset = offset;
        rt.paused_at = None;
        rt.paused_total = ShowTime::ZERO;
    }
    tracing::debug!(cue = %cue.header().number, kind = ?cue.kind(), ?offset, "cue start");
    cue.on_go(ctx, offset);
}

// ============================================================ concrete cues ==

/// Plays one or more sample-synchronous tracks (stems) plus an optional click.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioCue {
    #[serde(flatten)]
    pub header: CueHeader,
    pub tracks: Vec<AudioTrack>,
    #[serde(default)]
    pub click: Option<ClickTrack>,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub fade_in: ShowTime,
    #[serde(default)]
    pub fade_out: ShowTime,
    #[serde(default)]
    pub start_at: ShowTime,
    #[serde(default)]
    pub end_at: Option<ShowTime>,
    #[serde(default)]
    pub looped: bool,
    /// Length of the longest track, probed at import time.
    #[serde(default)]
    pub media_length: Option<ShowTime>,
}

impl Cue for AudioCue {
    fn header(&self) -> &CueHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut CueHeader {
        &mut self.header
    }
    fn kind(&self) -> CueKind {
        CueKind::Audio
    }
    fn duration(&self) -> Option<ShowTime> {
        if self.looped {
            return None;
        }
        let end = self.end_at.or(self.media_length)?;
        Some(end - self.start_at)
    }
    fn prepare(&mut self, ctx: &mut dyn CueContext) {
        let assets = self.tracks.iter().map(|t| t.asset.clone()).collect();
        ctx.dispatch(EngineAction::Audio(AudioAction::Preload { assets }));
    }
    fn on_go(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) {
        ctx.dispatch(EngineAction::Audio(AudioAction::Play {
            cue: self.header.id,
            tracks: self.tracks.clone(),
            click: self.click.clone(),
            gain_db: self.gain_db,
            fade_in: if offset.is_zero() { self.fade_in } else { ShowTime::ZERO },
            start_at: self.start_at + offset,
            end_at: self.end_at,
            looped: self.looped,
        }));
    }
    fn on_stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        let fade = fade.unwrap_or(self.fade_out);
        ctx.dispatch(EngineAction::Audio(AudioAction::Stop { cue: self.header.id, fade }));
    }
    fn on_pause(&mut self, ctx: &mut dyn CueContext) {
        ctx.dispatch(EngineAction::Audio(AudioAction::Pause { cue: self.header.id }));
    }
    fn on_seek(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) -> bool {
        ctx.dispatch(EngineAction::Audio(AudioAction::Seek { cue: self.header.id, position: offset }));
        true
    }
    fn on_resume(&mut self, ctx: &mut dyn CueContext) {
        ctx.dispatch(EngineAction::Audio(AudioAction::Resume { cue: self.header.id }));
    }
    fn media_clock(&self, ctx: &dyn CueContext) -> Option<ShowTime> {
        ctx.media_position(self.header.id)
    }
    fn media_finished(&self, ctx: &dyn CueContext) -> bool {
        !self.looped && ctx.media_finished(self.header.id)
    }
    fn validate(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.tracks.is_empty() && self.click.is_none() {
            v.push("audio cue has no tracks".into());
        }
        if let Some(end) = self.end_at {
            if end <= self.start_at {
                v.push("end time is before start time".into());
            }
        }
        v
    }
}

/// What a visual cue does when its media ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndBehavior {
    #[default]
    Hide,
    HoldLastFrame,
}

/// Shows any visual content (video, image, text/teleprompter, NDI, colour)
/// on any logical output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoCue {
    #[serde(flatten)]
    pub header: CueHeader,
    pub output: OutputId,
    pub content: VisualContent,
    #[serde(default)]
    pub layer: i32,
    #[serde(default = "full")]
    pub opacity: f32,
    #[serde(default)]
    pub fade_in: ShowTime,
    #[serde(default)]
    pub fade_out: ShowTime,
    /// For stills/text: how long to hold before completing (None = forever).
    #[serde(default)]
    pub hold: Option<ShowTime>,
    #[serde(default)]
    pub media_length: Option<ShowTime>,
    #[serde(default)]
    pub end_behavior: EndBehavior,
}
fn full() -> f32 {
    1.0
}

impl Cue for VideoCue {
    fn header(&self) -> &CueHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut CueHeader {
        &mut self.header
    }
    fn kind(&self) -> CueKind {
        CueKind::Video
    }
    fn duration(&self) -> Option<ShowTime> {
        match &self.content {
            VisualContent::Video { looped: false, .. } => self.media_length.or(self.hold),
            VisualContent::Video { looped: true, .. } => None,
            _ => self.hold,
        }
    }
    fn prepare(&mut self, ctx: &mut dyn CueContext) {
        let assets = match &self.content {
            VisualContent::Video { asset, .. } | VisualContent::Image { asset } => vec![asset.clone()],
            _ => return,
        };
        ctx.dispatch(EngineAction::Video(VideoAction::Preload { assets }));
    }
    fn on_go(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) {
        ctx.dispatch(EngineAction::Video(VideoAction::Show {
            cue: self.header.id,
            output: self.output,
            content: self.content.clone(),
            layer: self.layer,
            opacity: self.opacity,
            fade_in: if offset.is_zero() { self.fade_in } else { ShowTime::ZERO },
            start_at: offset,
        }));
    }
    fn on_stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        let fade = fade.unwrap_or(self.fade_out);
        ctx.dispatch(EngineAction::Video(VideoAction::Hide { cue: self.header.id, fade }));
    }
    fn on_complete(&mut self, ctx: &mut dyn CueContext) {
        if self.end_behavior == EndBehavior::Hide {
            ctx.dispatch(EngineAction::Video(VideoAction::Hide { cue: self.header.id, fade: self.fade_out }));
        }
    }
    fn on_pause(&mut self, ctx: &mut dyn CueContext) {
        ctx.dispatch(EngineAction::Video(VideoAction::Pause { cue: self.header.id }));
    }
    fn on_seek(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) -> bool {
        // Re-times the layer: the decoder seeks, teleprompters jump their scroll.
        ctx.dispatch(EngineAction::Video(VideoAction::Seek { cue: self.header.id, position: offset }));
        true
    }
    fn on_resume(&mut self, ctx: &mut dyn CueContext) {
        ctx.dispatch(EngineAction::Video(VideoAction::Resume { cue: self.header.id }));
    }
    fn media_clock(&self, ctx: &dyn CueContext) -> Option<ShowTime> {
        ctx.media_position(self.header.id)
    }
}

/// A show timer (count-up, countdown, time of day) routed to any output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimerCue {
    #[serde(flatten)]
    pub header: CueHeader,
    pub output: OutputId,
    pub timer: TimerContent,
    #[serde(default)]
    pub layer: i32,
    /// Keep showing `00:00` after a countdown completes.
    #[serde(default = "yes")]
    pub hold_at_zero: bool,
}

impl Cue for TimerCue {
    fn header(&self) -> &CueHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut CueHeader {
        &mut self.header
    }
    fn kind(&self) -> CueKind {
        CueKind::Timer
    }
    fn duration(&self) -> Option<ShowTime> {
        match self.timer.mode {
            TimerMode::CountDown { from } => Some(from),
            _ => None,
        }
    }
    fn on_go(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) {
        let mut timer = self.timer.clone();
        timer.started_at = ctx.now();
        timer.elapsed_offset = offset;
        ctx.dispatch(EngineAction::Video(VideoAction::Show {
            cue: self.header.id,
            output: self.output,
            content: VisualContent::Timer(timer),
            layer: self.layer,
            opacity: 1.0,
            fade_in: ShowTime::ZERO,
            start_at: offset,
        }));
    }
    fn on_stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        ctx.dispatch(EngineAction::Video(VideoAction::Hide { cue: self.header.id, fade: fade.unwrap_or_default() }));
    }
    fn on_seek(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) -> bool {
        // The compositor re-bases the timer's start so the display jumps too.
        ctx.dispatch(EngineAction::Video(VideoAction::Seek { cue: self.header.id, position: offset }));
        true
    }
    fn on_complete(&mut self, ctx: &mut dyn CueContext) {
        if !self.hold_at_zero {
            ctx.dispatch(EngineAction::Video(VideoAction::Hide { cue: self.header.id, fade: ShowTime::ZERO }));
        }
    }
}

/// A lighting look: raw channels and/or fixture attributes with a fade.
/// Values *track* (persist) after completion until released.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DmxCue {
    #[serde(flatten)]
    pub header: CueHeader,
    #[serde(default)]
    pub channels: Vec<DmxChannelValue>,
    #[serde(default)]
    pub attributes: Vec<FixtureAttributeValue>,
    #[serde(default)]
    pub fade: ShowTime,
    /// Release the look when the cue is stopped.
    #[serde(default = "yes")]
    pub release_on_stop: bool,
    /// Palettes resolved when the cue runs (explicit attributes win).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub palettes: Vec<crate::lighting::PaletteRef>,
    /// Lighting effects started with the look and stopped on release.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<crate::id::EffectId>,
}

impl Cue for DmxCue {
    fn header(&self) -> &CueHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut CueHeader {
        &mut self.header
    }
    fn kind(&self) -> CueKind {
        CueKind::Dmx
    }
    fn duration(&self) -> Option<ShowTime> {
        Some(self.fade)
    }
    fn on_go(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) {
        ctx.dispatch(EngineAction::Dmx(DmxAction::ApplyScene {
            cue: self.header.id,
            channels: self.channels.clone(),
            attributes: self.attributes.clone(),
            palettes: self.palettes.clone(),
            effects: self.effects.clone(),
            fade: self.fade - offset,
        }));
    }
    fn on_stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        if self.release_on_stop {
            ctx.dispatch(EngineAction::Dmx(DmxAction::Release { cue: self.header.id, fade: fade.unwrap_or_default() }));
        }
    }
    fn validate(&self) -> Vec<String> {
        self.channels
            .iter()
            .filter(|c| !(1..=512).contains(&c.channel))
            .map(|c| format!("channel {} out of range 1..=512", c.channel))
            .collect()
    }
}

/// Instant trigger: OSC, MIDI, or another show command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlCue {
    #[serde(flatten)]
    pub header: CueHeader,
    pub action: ControlAction,
}

impl Cue for ControlCue {
    fn header(&self) -> &CueHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut CueHeader {
        &mut self.header
    }
    fn kind(&self) -> CueKind {
        CueKind::Control
    }
    fn duration(&self) -> Option<ShowTime> {
        Some(ShowTime::ZERO)
    }
    fn on_go(&mut self, ctx: &mut dyn CueContext, _offset: ShowTime) {
        ctx.dispatch(EngineAction::Control(self.action.clone()));
    }
    fn on_stop(&mut self, _ctx: &mut dyn CueContext, _fade: Option<ShowTime>) {}
    fn chase_on_catch_up(&self) -> bool {
        false
    }
}

/// One page of a rendered presentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Slide {
    /// Rendered picture (PNG inside the bundle).
    pub image: crate::project::AssetRef,
    /// Text found on the slide (previews, search, confidence monitors).
    #[serde(default)]
    pub text: String,
    /// Speaker notes (PowerPoint).
    #[serde(default)]
    pub notes: String,
}

/// A PDF / PowerPoint document rendered to one image per slide at import.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SlideDeck {
    /// The imported original document.
    pub source: Option<crate::project::AssetRef>,
    /// Pixel size the slides were rendered at.
    pub size: [u32; 2],
    pub slides: Vec<Slide>,
}

/// A presentation routed to an output, navigated slide by slide
/// (Next / Previous / Go to) while it is running.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlideCue {
    #[serde(flatten)]
    pub header: CueHeader,
    pub output: OutputId,
    pub deck: SlideDeck,
    /// Slide shown at GO (0-based).
    #[serde(default)]
    pub start_slide: usize,
    #[serde(default)]
    pub layer: i32,
    #[serde(default = "full")]
    pub opacity: f32,
    #[serde(default)]
    pub fade_in: ShowTime,
    #[serde(default)]
    pub fade_out: ShowTime,
    /// Cross-fade between slides.
    #[serde(default)]
    pub transition: ShowTime,
    /// Slide currently on screen (runtime).
    #[serde(skip)]
    pub current: usize,
}

impl SlideCue {
    pub fn slide_count(&self) -> usize {
        self.deck.slides.len()
    }

    /// Moves to slide `index` (clamped). Returns false if nothing changed.
    pub fn goto(&mut self, ctx: &mut dyn CueContext, index: usize) -> bool {
        let Some(last) = self.slide_count().checked_sub(1) else { return false };
        let index = index.min(last);
        if index == self.current {
            return false;
        }
        self.current = index;
        if self.header.runtime.state.is_active() {
            ctx.dispatch(EngineAction::Video(VideoAction::GotoSlide { cue: self.header.id, index, fade: self.transition }));
        }
        true
    }

    pub fn next(&mut self, ctx: &mut dyn CueContext) -> bool {
        self.goto(ctx, self.current + 1)
    }

    pub fn previous(&mut self, ctx: &mut dyn CueContext) -> bool {
        match self.current.checked_sub(1) {
            Some(i) => self.goto(ctx, i),
            None => false,
        }
    }

    /// The slide after the current one, if any.
    pub fn upcoming(&self) -> Option<&Slide> {
        self.deck.slides.get(self.current + 1)
    }
}

impl Cue for SlideCue {
    fn header(&self) -> &CueHeader {
        &self.header
    }
    fn header_mut(&mut self) -> &mut CueHeader {
        &mut self.header
    }
    fn kind(&self) -> CueKind {
        CueKind::Slides
    }
    fn duration(&self) -> Option<ShowTime> {
        // A presentation runs until stopped.
        None
    }
    fn prepare(&mut self, ctx: &mut dyn CueContext) {
        self.current = self.start_slide.min(self.slide_count().saturating_sub(1));
        let first = self.deck.slides.iter().skip(self.current).take(2).map(|s| s.image.clone()).collect();
        ctx.dispatch(EngineAction::Video(VideoAction::Preload { assets: first }));
    }
    fn on_go(&mut self, ctx: &mut dyn CueContext, _offset: ShowTime) {
        self.current = self.start_slide.min(self.slide_count().saturating_sub(1));
        ctx.dispatch(EngineAction::Video(VideoAction::Show {
            cue: self.header.id,
            output: self.output,
            content: VisualContent::Slides { slides: self.deck.slides.iter().map(|s| s.image.clone()).collect(), index: self.current },
            layer: self.layer,
            opacity: self.opacity,
            fade_in: self.fade_in,
            start_at: ShowTime::ZERO,
        }));
    }
    fn on_stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        let fade = fade.unwrap_or(self.fade_out);
        ctx.dispatch(EngineAction::Video(VideoAction::Hide { cue: self.header.id, fade }));
    }
    fn chase_on_catch_up(&self) -> bool {
        true
    }
    fn validate(&self) -> Vec<String> {
        if self.deck.slides.is_empty() {
            vec!["presentation has no slides".into()]
        } else {
            Vec::new()
        }
    }
}

// ================================================================== AnyCue ==

/// Serialisable polymorphic cue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnyCue {
    Audio(AudioCue),
    Video(VideoCue),
    Timer(TimerCue),
    Dmx(DmxCue),
    Control(ControlCue),
    Timeline(crate::timeline::TimelineCue),
    Slides(SlideCue),
}

macro_rules! delegate {
    ($self:ident, $c:ident => $e:expr) => {
        match $self {
            AnyCue::Audio($c) => $e,
            AnyCue::Video($c) => $e,
            AnyCue::Timer($c) => $e,
            AnyCue::Dmx($c) => $e,
            AnyCue::Control($c) => $e,
            AnyCue::Timeline($c) => $e,
            AnyCue::Slides($c) => $e,
        }
    };
}

impl Cue for AnyCue {
    fn header(&self) -> &CueHeader {
        delegate!(self, c => c.header())
    }
    fn header_mut(&mut self) -> &mut CueHeader {
        delegate!(self, c => c.header_mut())
    }
    fn kind(&self) -> CueKind {
        delegate!(self, c => c.kind())
    }
    fn duration(&self) -> Option<ShowTime> {
        delegate!(self, c => c.duration())
    }
    fn prepare(&mut self, ctx: &mut dyn CueContext) {
        delegate!(self, c => c.prepare(ctx))
    }
    fn on_go(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) {
        delegate!(self, c => c.on_go(ctx, offset))
    }
    fn on_stop(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        delegate!(self, c => c.on_stop(ctx, fade))
    }
    fn on_pause(&mut self, ctx: &mut dyn CueContext) {
        delegate!(self, c => c.on_pause(ctx))
    }
    fn on_resume(&mut self, ctx: &mut dyn CueContext) {
        delegate!(self, c => c.on_resume(ctx))
    }
    fn on_seek(&mut self, ctx: &mut dyn CueContext, offset: ShowTime) -> bool {
        delegate!(self, c => c.on_seek(ctx, offset))
    }
    fn on_tick(&mut self, ctx: &mut dyn CueContext, elapsed: ShowTime) {
        delegate!(self, c => c.on_tick(ctx, elapsed))
    }
    fn on_complete(&mut self, ctx: &mut dyn CueContext) {
        delegate!(self, c => c.on_complete(ctx))
    }
    fn media_clock(&self, ctx: &dyn CueContext) -> Option<ShowTime> {
        delegate!(self, c => c.media_clock(ctx))
    }
    fn media_finished(&self, ctx: &dyn CueContext) -> bool {
        delegate!(self, c => c.media_finished(ctx))
    }
    fn chase_on_catch_up(&self) -> bool {
        delegate!(self, c => c.chase_on_catch_up())
    }
    fn validate(&self) -> Vec<String> {
        delegate!(self, c => c.validate())
    }
}

impl AnyCue {
    /// All assets referenced by this cue (recursing into timelines).
    pub fn assets(&self) -> Vec<&crate::project::AssetRef> {
        match self {
            AnyCue::Audio(a) => a.tracks.iter().map(|t| &t.asset).collect(),
            AnyCue::Video(v) => match &v.content {
                VisualContent::Video { asset, .. } | VisualContent::Image { asset } => vec![asset],
                _ => vec![],
            },
            AnyCue::Slides(s) => s.deck.source.iter().chain(s.deck.slides.iter().map(|x| &x.image)).collect(),
            AnyCue::Timeline(t) => {
                let mut out: Vec<_> = t.master.iter().flat_map(|m| m.assets()).collect();
                out.extend(t.items.iter().flat_map(|i| i.cue.assets()));
                out
            }
            _ => vec![],
        }
    }

    /// Depth-first search for a cue by id (including timeline children).
    pub fn find(&self, id: CueId) -> Option<&AnyCue> {
        if self.header().id == id {
            return Some(self);
        }
        if let AnyCue::Timeline(t) = self {
            if let Some(m) = t.master.as_deref().and_then(|m| m.find(id)) {
                return Some(m);
            }
            return t.items.iter().find_map(|i| i.cue.find(id));
        }
        None
    }
}

// ================================================================== tests ==

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Deterministic context with a manually advanced clock.
    #[derive(Default)]
    pub struct MockCtx {
        pub now: ShowTime,
        pub actions: Vec<EngineAction>,
        pub positions: std::collections::HashMap<CueId, ShowTime>,
        pub finished: std::collections::HashSet<CueId>,
    }
    impl MockCtx {
        pub fn advance(&mut self, t: ShowTime) {
            self.now += t;
        }
    }
    impl CueContext for MockCtx {
        fn now(&self) -> ShowTime {
            self.now
        }
        fn dispatch(&mut self, action: EngineAction) {
            self.actions.push(action);
        }
        fn media_position(&self, cue: CueId) -> Option<ShowTime> {
            self.positions.get(&cue).copied()
        }
        fn media_finished(&self, cue: CueId) -> bool {
            self.finished.contains(&cue)
        }
    }

    pub fn dmx_cue(number: &str, fade_ms: u64) -> AnyCue {
        AnyCue::Dmx(DmxCue {
            header: CueHeader::new(number, format!("Look {number}")),
            channels: vec![DmxChannelValue { universe: 0, channel: 1, value: 255 }],
            attributes: vec![],
            fade: ShowTime::from_millis(fade_ms),
            release_on_stop: true,
            palettes: vec![],
            effects: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn prewait_then_run_then_complete() {
        let mut ctx = MockCtx::default();
        let mut cue = dmx_cue("1", 1000);
        cue.header_mut().pre_wait = ShowTime::from_millis(500);
        cue.go(&mut ctx);
        assert_eq!(cue.state(), CueState::PreWait);
        assert!(ctx.actions.is_empty());
        ctx.advance(ShowTime::from_millis(500));
        assert_eq!(cue.tick(&mut ctx), Some(CueEvent::Started));
        assert_eq!(ctx.actions.len(), 1);
        ctx.advance(ShowTime::from_millis(400));
        assert_eq!(cue.tick(&mut ctx), None);
        assert_eq!(cue.elapsed(&ctx), ShowTime::from_millis(400));
        ctx.advance(ShowTime::from_millis(600));
        assert_eq!(cue.tick(&mut ctx), Some(CueEvent::Completed));
    }

    #[test]
    fn pause_excludes_time() {
        let mut ctx = MockCtx::default();
        let mut cue = dmx_cue("1", 10_000);
        cue.go(&mut ctx);
        ctx.advance(ShowTime::from_secs(1));
        cue.pause(&mut ctx);
        ctx.advance(ShowTime::from_secs(5));
        assert_eq!(cue.elapsed(&ctx), ShowTime::from_secs(1));
        cue.resume(&mut ctx);
        ctx.advance(ShowTime::from_secs(1));
        assert_eq!(cue.elapsed(&ctx), ShowTime::from_secs(2));
    }

    #[test]
    fn anycue_json_roundtrip() {
        let cue = dmx_cue("4.5", 250);
        let json = serde_json::to_string(&cue).unwrap();
        assert!(json.contains("\"type\":\"dmx\""));
        let back: AnyCue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cue);
    }

    fn video_cue() -> AnyCue {
        AnyCue::Video(VideoCue {
            header: CueHeader::new("2", "Clip"),
            output: OutputId::new(),
            content: VisualContent::Video { asset: crate::project::AssetRef::new("assets/video/a.mov"), looped: false },
            layer: 0,
            opacity: 1.0,
            fade_in: ShowTime::ZERO,
            fade_out: ShowTime::ZERO,
            hold: None,
            media_length: Some(ShowTime::from_secs(60)),
            end_behavior: EndBehavior::Hide,
        })
    }

    #[test]
    fn seek_while_playing_moves_in_place() {
        let mut ctx = MockCtx::default();
        let mut cue = video_cue();
        let id = cue.header().id;
        cue.go(&mut ctx);
        ctx.advance(ShowTime::from_secs(5));
        ctx.actions.clear();
        cue.seek(&mut ctx, ShowTime::from_secs(42));
        // No stop / restart: just one seek for the engine.
        assert_eq!(ctx.actions, vec![EngineAction::Video(VideoAction::Seek { cue: id, position: ShowTime::from_secs(42) })]);
        assert_eq!(cue.state(), CueState::Running);
        ctx.advance(ShowTime::from_secs(1));
        assert_eq!(cue.elapsed(&ctx), ShowTime::from_secs(43));
        // Seeking past the end stays inside the cue instead of completing it.
        cue.seek(&mut ctx, ShowTime::from_secs(500));
        assert!(cue.elapsed(&ctx) < ShowTime::from_secs(60));
        assert_eq!(cue.tick(&mut ctx), None);
    }

    #[test]
    fn seek_while_paused_stays_paused() {
        let mut ctx = MockCtx::default();
        let mut cue = video_cue();
        cue.go(&mut ctx);
        ctx.advance(ShowTime::from_secs(2));
        cue.pause(&mut ctx);
        cue.seek(&mut ctx, ShowTime::from_secs(10));
        assert_eq!(cue.state(), CueState::Paused);
        ctx.advance(ShowTime::from_secs(3));
        assert_eq!(cue.elapsed(&ctx), ShowTime::from_secs(10));
        cue.resume(&mut ctx);
        ctx.advance(ShowTime::from_secs(1));
        assert_eq!(cue.elapsed(&ctx), ShowTime::from_secs(11));
    }

    fn deck(n: usize) -> SlideCue {
        SlideCue {
            header: CueHeader::new("5", "Keynote"),
            output: OutputId::new(),
            deck: SlideDeck {
                source: None,
                size: [1920, 1080],
                slides: (0..n)
                    .map(|i| Slide { image: crate::project::AssetRef::new(format!("assets/slides/k/{i:03}.png")), text: format!("Slide {i}"), notes: String::new() })
                    .collect(),
            },
            start_slide: 1,
            layer: 0,
            opacity: 1.0,
            fade_in: ShowTime::ZERO,
            fade_out: ShowTime::ZERO,
            transition: ShowTime::from_millis(300),
            current: 0,
        }
    }

    #[test]
    fn slides_navigate_and_clamp() {
        let mut ctx = MockCtx::default();
        let mut cue = deck(3);
        let id = cue.header.id;
        cue.go(&mut ctx);
        assert!(matches!(&ctx.actions[0], EngineAction::Video(VideoAction::Show { content: VisualContent::Slides { index: 1, slides }, .. }) if slides.len() == 3));
        assert_eq!(cue.upcoming().map(|s| s.text.as_str()), Some("Slide 2"));
        ctx.actions.clear();
        assert!(cue.next(&mut ctx));
        assert!(!cue.next(&mut ctx), "already on the last slide");
        assert_eq!(ctx.actions, vec![EngineAction::Video(VideoAction::GotoSlide { cue: id, index: 2, fade: ShowTime::from_millis(300) })]);
        assert!(cue.previous(&mut ctx) && cue.previous(&mut ctx));
        assert!(!cue.previous(&mut ctx));
        assert_eq!(cue.current, 0);
        let json = serde_json::to_string(&AnyCue::Slides(cue.clone())).unwrap();
        assert!(json.contains("\"type\":\"slides\""));
    }
}
