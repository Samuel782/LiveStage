//! The real-time side of the engine.
//!
//! Everything in this file that is reachable from [`Renderer::render`] must
//! stay allocation-, lock- and syscall-free. Objects arrive pre-built in
//! [`RtCommand`]s and leave through the garbage queue.

use crate::metronome::Metronome;
use crate::routing::RoutingTable;
use crate::AudioClip;
use livestage_core::id::{CueId, StemId};
use ringbuf::traits::{Consumer, Producer};
use ringbuf::{HeapCons, HeapProd};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Maximum simultaneous audio cues.
pub const MAX_GROUPS: usize = 128;
/// Maximum physical output channels metered.
pub const MAX_CHANNELS: usize = 64;
/// Fade used for "instant" stops / pauses to avoid clicks (~5 ms @ 48k).
pub const DECLICK_FRAMES: u32 = 240;

/// Per-cue status published by the audio thread.
#[derive(Debug, Default)]
pub struct VoiceShared {
    /// Frames played since the cue's audio start (after count-in), i.e. the
    /// cue's sample clock.
    pub position_frames: AtomicU64,
    pub finished: AtomicBool,
    pub started: AtomicBool,
    /// True while audibly playing (not paused / pausing); gates clock interpolation.
    pub playing: AtomicBool,
}

/// Engine-wide status published by the audio thread.
#[derive(Debug)]
pub struct EngineShared {
    /// Peak per output since last read, as f32 bits.
    pub peaks: [AtomicU32; MAX_CHANNELS],
    pub callbacks: AtomicU64,
    pub frames_rendered: AtomicU64,
    /// Commands that could not be honoured (no free voice slot, ...).
    pub rejected: AtomicU64,
    /// Garbage that could not be returned and was leaked instead of freed on
    /// the RT thread.
    pub leaked: AtomicU64,
    pub active_groups: AtomicU32,
    /// DSP load of the last callback in 1/1000ths of the buffer period.
    pub load_permille: AtomicU32,
    /// Reference instant for `callback_at_ns`.
    pub epoch: std::time::Instant,
    /// When the last device callback finished, in ns since `epoch` (0 = never).
    pub callback_at_ns: AtomicU64,
    /// Frames rendered by the last device callback.
    pub callback_frames: AtomicU32,
}

impl Default for EngineShared {
    fn default() -> Self {
        Self {
            peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            callbacks: AtomicU64::new(0),
            frames_rendered: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            leaked: AtomicU64::new(0),
            active_groups: AtomicU32::new(0),
            load_permille: AtomicU32::new(0),
            epoch: std::time::Instant::now(),
            callback_at_ns: AtomicU64::new(0),
            callback_frames: AtomicU32::new(0),
        }
    }
}

impl EngineShared {
    /// Reads and resets the peak of an output.
    pub fn take_peak(&self, ch: usize) -> f32 {
        self.peaks.get(ch).map(|p| f32::from_bits(p.swap(0, Ordering::Relaxed))).unwrap_or(0.0)
    }
}

/// Linear gain ramp evaluated per frame.
#[derive(Debug, Clone, Copy)]
pub struct Ramp {
    pub value: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl Ramp {
    pub fn new(value: f32) -> Self {
        Self { value, target: value, step: 0.0, remaining: 0 }
    }
    pub fn to(&mut self, target: f32, frames: u32) {
        let frames = frames.max(1);
        self.target = target;
        self.step = (target - self.value) / frames as f32;
        self.remaining = frames;
    }
    #[inline]
    pub fn advance(&mut self) -> f32 {
        if self.remaining > 0 {
            self.value += self.step;
            self.remaining -= 1;
            if self.remaining == 0 {
                self.value = self.target;
            }
        }
        self.value
    }
    pub fn is_settled(&self) -> bool {
        self.remaining == 0
    }
    pub fn target(&self) -> f32 {
        self.target
    }
}

#[derive(Debug)]
pub struct TrackVoice {
    pub clip: Arc<AudioClip>,
    pub stem: StemId,
    pub gain: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupState {
    Playing,
    /// Fading to silence, then parks in `Paused`.
    Pausing,
    Paused,
    /// Fading to silence, then is removed.
    Stopping,
}

/// All voices belonging to one cue, started on the same sample frame.
#[derive(Debug)]
pub struct VoiceGroup {
    pub cue: CueId,
    pub tracks: Vec<TrackVoice>,
    pub click: Option<(Metronome, StemId)>,
    /// Frames of click count-in before tracks start.
    pub count_in: u64,
    /// Frames rendered since group start (includes count-in).
    pub frame: u64,
    /// First source frame to play.
    pub start_frame: u64,
    /// Exclusive end source frame.
    pub end_frame: Option<u64>,
    pub looped: bool,
    pub gain: Ramp,
    /// Cue gain (target gain when resuming).
    pub level: f32,
    pub state: GroupState,
    pub shared: Arc<VoiceShared>,
}

impl VoiceGroup {
    fn source_len(&self) -> u64 {
        let longest = self.tracks.iter().map(|t| t.clip.frames()).max().unwrap_or(0);
        self.end_frame.map_or(longest, |e| e.min(longest))
    }
}

pub enum RtCommand {
    Start(Box<VoiceGroup>),
    Stop { cue: CueId, fade_frames: u32 },
    Pause { cue: CueId },
    Resume { cue: CueId },
    /// Jump to `position` frames into the cue's audio (after count-in).
    Seek { cue: CueId, position: u64 },
    SetGain { cue: CueId, gain: f32, fade_frames: u32 },
    StopAll { fade_frames: u32 },
    SetMasterGain { gain: f32, fade_frames: u32 },
    SetRouting(Box<RoutingTable>),
}

/// Objects handed back to the control thread to be dropped there.
pub enum Garbage {
    Group(Box<VoiceGroup>),
    Routing(Box<RoutingTable>),
}

pub struct Renderer {
    commands: HeapCons<RtCommand>,
    garbage: HeapProd<Garbage>,
    groups: Vec<Option<Box<VoiceGroup>>>,
    routing: Box<RoutingTable>,
    master: Ramp,
    channels: usize,
    sample_rate: u32,
    shared: Arc<EngineShared>,
    /// Per-output peak accumulator for the current callback.
    peak_scratch: [f32; MAX_CHANNELS],
}

impl Renderer {
    pub(crate) fn new(
        commands: HeapCons<RtCommand>,
        garbage: HeapProd<Garbage>,
        routing: Box<RoutingTable>,
        channels: usize,
        sample_rate: u32,
        shared: Arc<EngineShared>,
    ) -> Self {
        let mut groups = Vec::with_capacity(MAX_GROUPS);
        groups.resize_with(MAX_GROUPS, || None);
        Self {
            commands,
            garbage,
            groups,
            routing,
            master: Ramp::new(1.0),
            channels,
            sample_rate,
            shared,
            peak_scratch: [0.0; MAX_CHANNELS],
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn trash(&mut self, g: Garbage) {
        if let Err(g) = self.garbage.try_push(g) {
            // Never free on the audio thread: leak instead and report it.
            std::mem::forget(g);
            self.shared.leaked.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn group_mut(&mut self, cue: CueId) -> impl Iterator<Item = &mut Box<VoiceGroup>> {
        self.groups.iter_mut().flatten().filter(move |g| g.cue == cue)
    }

    fn apply_commands(&mut self) {
        while let Some(cmd) = self.commands.try_pop() {
            match cmd {
                RtCommand::Start(group) => {
                    if let Some(slot) = self.groups.iter_mut().find(|s| s.is_none()) {
                        group.shared.started.store(true, Ordering::Release);
                        *slot = Some(group);
                    } else {
                        self.shared.rejected.fetch_add(1, Ordering::Relaxed);
                        self.trash(Garbage::Group(group));
                    }
                }
                RtCommand::Stop { cue, fade_frames } => {
                    for g in self.group_mut(cue) {
                        g.state = GroupState::Stopping;
                        g.gain.to(0.0, fade_frames.max(DECLICK_FRAMES));
                    }
                }
                RtCommand::Pause { cue } => {
                    for g in self.group_mut(cue) {
                        if g.state == GroupState::Playing {
                            g.state = GroupState::Pausing;
                            g.gain.to(0.0, DECLICK_FRAMES);
                        }
                    }
                }
                RtCommand::Resume { cue } => {
                    for g in self.group_mut(cue) {
                        if matches!(g.state, GroupState::Paused | GroupState::Pausing) {
                            g.state = GroupState::Playing;
                            let level = g.level;
                            g.gain.to(level, DECLICK_FRAMES);
                        }
                    }
                }
                RtCommand::Seek { cue, position } => {
                    for g in self.group_mut(cue) {
                        g.frame = g.count_in + position;
                        let beat_frame = g.frame;
                        if let Some((m, _)) = g.click.as_mut() {
                            m.seek(beat_frame);
                        }
                    }
                }
                RtCommand::SetGain { cue, gain, fade_frames } => {
                    for g in self.group_mut(cue) {
                        g.level = gain;
                        if g.state == GroupState::Playing {
                            g.gain.to(gain, fade_frames.max(1));
                        }
                    }
                }
                RtCommand::StopAll { fade_frames } => {
                    for g in self.groups.iter_mut().flatten() {
                        g.state = GroupState::Stopping;
                        g.gain.to(0.0, fade_frames.max(DECLICK_FRAMES));
                    }
                }
                RtCommand::SetMasterGain { gain, fade_frames } => self.master.to(gain, fade_frames.max(1)),
                RtCommand::SetRouting(table) => {
                    let old = std::mem::replace(&mut self.routing, table);
                    self.trash(Garbage::Routing(old));
                }
            }
        }
    }

    /// Renders one interleaved buffer of `out.len() / channels` frames.
    pub fn render(&mut self, out: &mut [f32]) {
        self.apply_commands();
        out.fill(0.0);
        let ch = self.channels;
        if ch == 0 {
            return;
        }
        let frames = out.len() / ch;

        for slot_idx in 0..self.groups.len() {
            let Some(group) = self.groups[slot_idx].as_mut() else { continue };
            let outcome = mix_group(group, &self.routing, out, ch, frames);
            if outcome != MixOutcome::Continue {
                if outcome == MixOutcome::Ended {
                    group.shared.finished.store(true, Ordering::Release);
                }
                if let Some(g) = self.groups[slot_idx].take() {
                    self.trash(Garbage::Group(g));
                }
            }
        }

        // Master gain + peak metering.
        self.peak_scratch[..ch.min(MAX_CHANNELS)].fill(0.0);
        for f in 0..frames {
            let m = self.master.advance();
            for c in 0..ch {
                let s = &mut out[f * ch + c];
                *s = (*s * m).clamp(-1.0, 1.0);
                if c < MAX_CHANNELS {
                    self.peak_scratch[c] = self.peak_scratch[c].max(s.abs());
                }
            }
        }
        for c in 0..ch.min(MAX_CHANNELS) {
            let p = &self.shared.peaks[c];
            let prev = f32::from_bits(p.load(Ordering::Relaxed));
            if self.peak_scratch[c] > prev {
                p.store(self.peak_scratch[c].to_bits(), Ordering::Relaxed);
            }
        }
        let active = self.groups.iter().filter(|g| g.is_some()).count() as u32;
        self.shared.active_groups.store(active, Ordering::Relaxed);
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        self.shared.frames_rendered.fetch_add(frames as u64, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MixOutcome {
    Continue,
    /// Media reached its end naturally.
    Ended,
    /// Stop fade completed.
    Stopped,
}

/// Mixes one group into `out`.
#[inline]
fn mix_group(g: &mut VoiceGroup, routing: &RoutingTable, out: &mut [f32], ch: usize, frames: usize) -> MixOutcome {
    g.shared.playing.store(g.state == GroupState::Playing, Ordering::Relaxed);
    if g.state == GroupState::Paused {
        return MixOutcome::Continue;
    }
    let source_len = g.source_len();
    let play_len = source_len.saturating_sub(g.start_frame);
    let mut outcome = MixOutcome::Continue;

    for f in 0..frames {
        let gain = g.gain.advance();
        // Count-in + click.
        if let Some((m, stem_id)) = g.click.as_mut() {
            let s = m.next_sample() * gain;
            if s != 0.0 {
                if let Some(stem) = routing.stem(*stem_id) {
                    for &(_, o, rg) in &stem.routes {
                        out[f * ch + o as usize] += s * rg * stem.gain;
                    }
                }
            }
        }

        if g.frame >= g.count_in {
            let mut rel = g.frame - g.count_in;
            if rel >= play_len {
                if g.looped && play_len > 0 {
                    rel %= play_len;
                } else if !g.tracks.is_empty() {
                    outcome = MixOutcome::Ended;
                    break;
                }
                // Click-only groups run until stopped.
            }
            if rel < play_len || g.looped {
                let src = g.start_frame + rel;
                for t in &g.tracks {
                    let clip = &t.clip;
                    if src >= clip.frames() {
                        continue;
                    }
                    let Some(stem) = routing.stem(t.stem) else { continue };
                    let level = gain * t.gain * stem.gain;
                    if level == 0.0 {
                        continue;
                    }
                    let clip_ch = clip.channels.max(1);
                    for &(s_ch, o, rg) in &stem.routes {
                        // Up/down-mix the clip onto the stem's channel layout.
                        let v = if clip_ch >= stem.channels {
                            let mut acc = 0.0;
                            let mut n = 0u16;
                            let mut c = s_ch;
                            while c < clip_ch {
                                acc += clip.sample(src, c);
                                n += 1;
                                c += stem.channels;
                            }
                            if n > 0 { acc / n as f32 } else { 0.0 }
                        } else {
                            clip.sample(src, s_ch % clip_ch)
                        };
                        out[f * ch + o as usize] += v * level * rg;
                    }
                }
            }
            let pos = if g.looped && play_len > 0 { rel % play_len } else { rel };
            g.shared.position_frames.store(pos, Ordering::Relaxed);
        }
        if g.state != GroupState::Paused {
            g.frame += 1;
        }
        if g.gain.is_settled() && g.gain.value == 0.0 {
            match g.state {
                GroupState::Stopping => {
                    outcome = MixOutcome::Stopped;
                    break;
                }
                GroupState::Pausing => {
                    g.state = GroupState::Paused;
                    break;
                }
                _ => {}
            }
        }
    }
    outcome
}
