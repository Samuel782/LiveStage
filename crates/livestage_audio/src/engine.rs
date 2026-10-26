//! Control-thread side of the audio engine.

use crate::clip::AudioClip;
use crate::decode::decode_file;
use crate::device::{build_stream, select_device};
use crate::metronome::Metronome;
use crate::renderer::*;
use crate::routing::RoutingTable;
use crate::stream::{open_stream, StreamReader, StreamedClip, Streamer};
use crate::{db_to_gain, AudioResult};
use cpal::traits::StreamTrait;
use livestage_core::action::AudioAction;
use livestage_core::id::CueId;
use livestage_core::project::AssetRef;
use livestage_core::routing::AudioRouting;
use livestage_core::time::ShowTime;
use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};

/// Owns the thread holding the cpal stream.
struct AudioHost {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AudioHost {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

const COMMAND_QUEUE: usize = 1024;
const GARBAGE_QUEUE: usize = 1024;

#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    pub device_name: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_frames: Option<u32>,
    pub channels: Option<u16>,
}

impl EngineConfig {
    pub fn from_routing(r: &AudioRouting) -> Self {
        Self {
            device_name: r.device_name.clone(),
            sample_rate: r.sample_rate,
            buffer_frames: r.buffer_frames,
            channels: None,
        }
    }
}

/// A loaded file: decoded whole (short files) or streamed from disk with a
/// resident pre-roll (long files, see [`crate::stream`]).
#[derive(Debug, Clone)]
enum Clip {
    Resident(Arc<AudioClip>),
    Streamed(Arc<StreamedClip>),
}

#[derive(Debug, Clone)]
enum ClipSlot {
    Loading,
    Ready(Clip),
    Failed(String),
}

/// Clip cache with a background loader thread.
///
/// The mutex here is only ever touched by control/loader threads — never by
/// the audio thread, which receives clips and stream readers inside commands.
#[derive(Clone)]
struct ClipCache {
    slots: Arc<Mutex<HashMap<PathBuf, ClipSlot>>>,
    loader: mpsc::Sender<PathBuf>,
}

impl ClipCache {
    fn new(sample_rate: u32) -> Self {
        let slots: Arc<Mutex<HashMap<PathBuf, ClipSlot>>> = Default::default();
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let worker_slots = slots.clone();
        std::thread::Builder::new()
            .name("livestage-audio-loader".into())
            .spawn(move || {
                for path in rx {
                    let slot = load(&path, sample_rate);
                    worker_slots.lock().insert(path, slot);
                }
            })
            .expect("spawn audio loader");
        Self { slots, loader: tx }
    }

    fn preload(&self, path: &Path) {
        let mut slots = self.slots.lock();
        if !slots.contains_key(path) {
            slots.insert(path.to_path_buf(), ClipSlot::Loading);
            let _ = self.loader.send(path.to_path_buf());
        }
    }

    /// The clip once decoded, its error if decoding failed, or `None` while
    /// the loader thread works on it (queued now if nobody asked before).
    /// Never decodes on the calling thread.
    fn poll(&self, path: &Path) -> Option<Result<Clip, String>> {
        let mut slots = self.slots.lock();
        match slots.get(path) {
            Some(ClipSlot::Ready(c)) => Some(Ok(c.clone())),
            Some(ClipSlot::Failed(e)) => Some(Err(e.clone())),
            Some(ClipSlot::Loading) => None,
            None => {
                slots.insert(path.to_path_buf(), ClipSlot::Loading);
                let _ = self.loader.send(path.to_path_buf());
                None
            }
        }
    }

    fn insert(&self, path: PathBuf, clip: Arc<AudioClip>) {
        self.slots.lock().insert(path, ClipSlot::Ready(Clip::Resident(clip)));
    }
}

fn load(path: &Path, sample_rate: u32) -> ClipSlot {
    // Long files: probe, keep only the pre-roll, stream the rest.
    match StreamedClip::open(path, sample_rate) {
        Ok(Some(clip)) => {
            tracing::info!(
                file = %path.display(),
                secs = clip.duration_secs(),
                resident_mb = clip.resident_bytes() as f64 / 1e6,
                "audio streamed from disk"
            );
            return ClipSlot::Ready(Clip::Streamed(Arc::new(clip)));
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(file = %path.display(), "streaming probe failed, decoding whole: {e}"),
    }
    match decode_file(path) {
        Ok(clip) => {
            let clip = clip.resampled(sample_rate);
            tracing::info!(file = %path.display(), secs = clip.duration_secs(), "audio loaded");
            ClipSlot::Ready(Clip::Resident(Arc::new(clip)))
        }
        Err(e) => {
            tracing::error!("{e}");
            ClipSlot::Failed(e.to_string())
        }
    }
}

/// One reading of a cue's sample clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// Media time of the cue's audio (zero during a count-in).
    pub position: ShowTime,
    /// The clock is advancing: started, audibly playing and not finished.
    pub running: bool,
    /// The cue's audio reached its end (or was stopped and released).
    pub ended: bool,
}

/// A cue's sample clock, readable from any thread without locks. Stays
/// valid after the voice ends (frozen at its last position).
#[derive(Clone)]
pub struct VoiceClock {
    voice: Arc<VoiceShared>,
    engine: Arc<EngineShared>,
    sample_rate: u32,
}

impl VoiceClock {
    pub fn read(&self) -> ClockReading {
        let v = &self.voice;
        if v.seek_pending.load(Ordering::Acquire) {
            let position = v.seek_target.load(Ordering::Relaxed);
            return ClockReading { position: ShowTime::from_frames(position, self.sample_rate), running: false, ended: false };
        }
        if !v.started.load(Ordering::Acquire) {
            return ClockReading { position: ShowTime::ZERO, running: false, ended: false };
        }
        let ended = v.finished.load(Ordering::Acquire);
        let running = v.playing.load(Ordering::Relaxed) && !ended;
        let extra = if running { self.engine.frames_since_callback(self.sample_rate).unwrap_or(0) } else { 0 };
        let frames = v.position_frames.load(Ordering::Relaxed) + extra;
        ClockReading { position: ShowTime::from_frames(frames, self.sample_rate), running, ended }
    }
}

/// A GO whose files are still being decoded. Its clock is registered (held
/// at zero) so timelines and frame-locked video wait for it; it starts on the
/// first [`AudioEngine::maintenance`] after the loader delivers.
struct WaitingPlay {
    cue: CueId,
    play: AudioAction,
    /// Resolved file per track (`None`: muted or unresolvable).
    paths: Vec<Option<PathBuf>>,
    shared: Arc<VoiceShared>,
    /// Pause / resume / seek / gain changes received meanwhile, replayed in
    /// order once the voice starts.
    deferred: Vec<AudioAction>,
    /// Tracks, once every clip is loaded; streams in it prime before the start.
    voices: Option<Vec<TrackVoice>>,
}

/// A playing cue with streamed tracks: what a seek needs to open new streams.
struct StreamedPlay {
    shared: Arc<VoiceShared>,
    /// Per group track: its streamed clip (`None` for resident tracks).
    tracks: Vec<Option<Arc<StreamedClip>>>,
    start_frame: u64,
    end_frame: Option<u64>,
    looped: bool,
    play_len: u64,
}

impl StreamedPlay {
    /// Opens streams at `position` frames into the cue's audio.
    fn open_at(&self, position: u64, engine: &Arc<EngineShared>, streamer: &Streamer) -> Vec<Option<StreamReader>> {
        let rel = if self.looped && self.play_len > 0 { position % self.play_len } else { position };
        let src = self.start_frame + rel;
        let region = (self.start_frame, self.end_frame.unwrap_or(u64::MAX));
        self.tracks
            .iter()
            .map(|clip| {
                let clip = clip.as_ref()?;
                match open_stream(clip, src, region, self.looped, engine) {
                    Ok((reader, job)) => {
                        streamer.submit(job);
                        Some(reader)
                    }
                    Err(e) => {
                        tracing::error!(file = %clip.path.display(), "cannot reopen stream: {e}");
                        None
                    }
                }
            })
            .collect()
    }
}

/// A seek on a cue with streamed tracks, waiting for its new streams to prime.
struct WaitingSeek {
    cue: CueId,
    position: u64,
    streams: Vec<Option<StreamReader>>,
}

/// Owner of the audio pipeline. Lives on the control thread.
pub struct AudioEngine {
    tx: HeapProd<RtCommand>,
    garbage: HeapCons<Garbage>,
    shared: Arc<EngineShared>,
    voices: HashMap<CueId, Arc<VoiceShared>>,
    waiting: Vec<WaitingPlay>,
    streamed: HashMap<CueId, StreamedPlay>,
    seeks: Vec<WaitingSeek>,
    streamer: Streamer,
    /// Underruns already reported in the log.
    underruns_logged: u64,
    cache: ClipCache,
    sample_rate: u32,
    channels: u16,
    device_name: String,
    /// Thread that owns the (`!Send`) cpal stream; dropping it stops audio.
    host: Option<AudioHost>,
    /// Crosspoints skipped because the device has too few outputs.
    dropped_routes: Vec<(String, u16)>,
}

impl AudioEngine {
    fn build(sample_rate: u32, channels: u16, routing: &AudioRouting, device_name: String) -> (Self, Renderer) {
        let (tx, rx) = HeapRb::<RtCommand>::new(COMMAND_QUEUE).split();
        let (gtx, grx) = HeapRb::<Garbage>::new(GARBAGE_QUEUE).split();
        let shared = Arc::new(EngineShared::default());
        let (table, dropped) = RoutingTable::compile(routing, channels);
        let table = Box::new(table);
        let renderer = Renderer::new(rx, gtx, table, channels as usize, shared.clone());
        let engine = Self {
            tx,
            garbage: grx,
            shared,
            voices: HashMap::new(),
            waiting: Vec::new(),
            streamed: HashMap::new(),
            seeks: Vec::new(),
            streamer: Streamer::spawn(),
            underruns_logged: 0,
            cache: ClipCache::new(sample_rate),
            sample_rate,
            channels,
            device_name,
            host: None,
            dropped_routes: Vec::new(),
        };
        let mut engine = engine;
        engine.report_dropped(dropped);
        (engine, renderer)
    }

    /// An engine without a device; the caller drives the [`Renderer`]
    /// (tests, offline bounce, or a custom backend such as JACK).
    pub fn offline(sample_rate: u32, channels: u16, routing: &AudioRouting) -> (Self, Renderer) {
        Self::build(sample_rate, channels, routing, "Offline".into())
    }

    /// Opens the configured device and starts the real-time stream.
    ///
    /// `cpal::Stream` is `!Send`, so the stream lives on a dedicated host
    /// thread. That keeps `AudioEngine` itself `Send`, so it can be owned by
    /// the show thread.
    pub fn start(config: &EngineConfig, routing: &AudioRouting) -> AudioResult<Self> {
        let (info_tx, info_rx) = mpsc::channel::<AudioResult<(u32, u16, String)>>();
        let (renderer_tx, renderer_rx) = mpsc::channel::<(Renderer, Arc<EngineShared>)>();
        let (ready_tx, ready_rx) = mpsc::channel::<AudioResult<()>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let cfg = config.clone();
        let thread = std::thread::Builder::new()
            .name("livestage-audio-host".into())
            .spawn(move || {
                let opened = match select_device(cfg.device_name.as_deref(), cfg.sample_rate, cfg.buffer_frames, cfg.channels) {
                    Ok(o) => o,
                    Err(e) => return drop(info_tx.send(Err(e))),
                };
                let _ = info_tx.send(Ok((opened.config.sample_rate.0, opened.config.channels, opened.name.clone())));
                let Ok((renderer, shared)) = renderer_rx.recv() else { return };
                let stream = match build_stream(&opened, renderer, shared) {
                    Ok(s) => s,
                    Err(e) => return drop(ready_tx.send(Err(e))),
                };
                if let Err(e) = stream.play() {
                    return drop(ready_tx.send(Err(crate::AudioError::Device(e.to_string()))));
                }
                tracing::info!(device = %opened.name, buffer = ?opened.config.buffer_size, "audio stream running");
                let _ = ready_tx.send(Ok(()));
                // Park until the engine is dropped (or the sender is gone).
                let _ = stop_rx.recv();
                drop(stream);
            })
            .map_err(crate::AudioError::Io)?;
        let disconnected = || crate::AudioError::Device("audio host thread exited".into());
        let (rate, ch, name) = info_rx.recv().map_err(|_| disconnected())??;
        let (mut engine, renderer) = Self::build(rate, ch, routing, name.clone());
        renderer_tx.send((renderer, engine.shared.clone())).map_err(|_| disconnected())?;
        ready_rx.recv().map_err(|_| disconnected())??;
        tracing::info!(device = %name, rate, channels = ch, "audio engine started");
        engine.host = Some(AudioHost { stop: Some(stop_tx), thread: Some(thread) });
        Ok(engine)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn device_name(&self) -> &str {
        &self.device_name
    }
    pub fn shared(&self) -> &EngineShared {
        &self.shared
    }

    fn send(&mut self, cmd: RtCommand) {
        if self.tx.try_push(cmd).is_err() {
            tracing::error!("audio command queue full; command dropped");
        }
    }

    fn frames(&self, t: ShowTime) -> u32 {
        t.to_frames(self.sample_rate).min(u32::MAX as u64) as u32
    }

    /// Recompiles and hot-swaps the patch bay (glitch-free).
    pub fn set_routing(&mut self, routing: &AudioRouting) {
        let (table, dropped) = RoutingTable::compile(routing, self.channels);
        self.report_dropped(dropped);
        self.send(RtCommand::SetRouting(Box::new(table)));
        let gain = db_to_gain(routing.master_gain_db);
        self.send(RtCommand::SetMasterGain { gain, fade_frames: 480 });
    }

    /// Logs skipped crosspoints once per distinct set (patch edits recompile often).
    fn report_dropped(&mut self, dropped: Vec<(String, u16)>) {
        if dropped == self.dropped_routes {
            return;
        }
        if !dropped.is_empty() {
            let list: Vec<String> = dropped.iter().map(|(stem, out)| format!("{stem}→{out}")).collect();
            tracing::warn!(
                device_channels = self.channels,
                "{} crosspoint(s) target outputs the device doesn't have and are ignored: {}",
                dropped.len(),
                list.join(", ")
            );
        }
        self.dropped_routes = dropped;
    }

    /// Makes a clip available under `path` without decoding (tests, generated audio).
    pub fn insert_clip(&self, path: impl Into<PathBuf>, clip: AudioClip) {
        self.cache.insert(path.into(), Arc::new(clip.resampled(self.sample_rate)));
    }

    /// Executes a core [`AudioAction`]. `resolve` maps bundle asset refs to
    /// absolute paths.
    pub fn handle(&mut self, action: &AudioAction, resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>) {
        if self.defer_for_waiting(action) {
            return;
        }
        match action {
            AudioAction::Play { cue, tracks, .. } => {
                let paths = tracks
                    .iter()
                    .map(|t| {
                        let p = if t.mute { None } else { resolve(&t.asset) };
                        if p.is_none() && !t.mute {
                            tracing::error!(asset = %t.asset, "cannot resolve audio asset");
                        }
                        p
                    })
                    .collect();
                self.waiting.retain(|w| w.cue != *cue);
                self.seeks.retain(|s| s.cue != *cue);
                self.streamed.remove(cue);
                let shared = Arc::new(VoiceShared::default());
                self.voices.insert(*cue, shared.clone());
                let mut w = WaitingPlay { cue: *cue, play: action.clone(), paths, shared, deferred: Vec::new(), voices: None };
                if !self.try_start(&mut w) {
                    if w.voices.is_some() {
                        tracing::debug!(cue = %cue, "priming disk stream before start");
                    } else {
                        tracing::warn!(cue = %cue, "audio not preloaded; starting once decoded");
                    }
                    self.waiting.push(w);
                }
            }
            AudioAction::Stop { cue, fade } => {
                self.seeks.retain(|s| s.cue != *cue);
                if let Some(p) = self.streamed.remove(cue) {
                    p.shared.seek_pending.store(false, Ordering::Release);
                }
                let fade_frames = self.frames(*fade);
                self.send(RtCommand::Stop { cue: *cue, fade_frames });
            }
            AudioAction::Pause { cue } => self.send(RtCommand::Pause { cue: *cue }),
            AudioAction::Resume { cue } => self.send(RtCommand::Resume { cue: *cue }),
            AudioAction::Seek { cue, position } => {
                let position = position.to_frames(self.sample_rate);
                if let Some(play) = self.streamed.get(cue) {
                    // New streams prime at the target while the old ones keep
                    // playing; the clock holds at the target until the splice.
                    play.shared.seek_target.store(position, Ordering::Relaxed);
                    play.shared.seek_pending.store(true, Ordering::Release);
                    let streams = play.open_at(position, &self.shared, &self.streamer);
                    self.seeks.retain(|s| s.cue != *cue);
                    self.seeks.push(WaitingSeek { cue: *cue, position, streams });
                    self.poll_seeks();
                    return;
                }
                if let Some(v) = self.voices.get(cue) {
                    v.position_frames.store(position, Ordering::Relaxed);
                }
                self.send(RtCommand::Seek { cue: *cue, position });
            }
            AudioAction::SetGain { cue, gain_db, fade } => {
                let fade_frames = self.frames(*fade);
                self.send(RtCommand::SetGain { cue: *cue, gain: db_to_gain(*gain_db), fade_frames });
            }
            AudioAction::Preload { assets } => {
                for a in assets {
                    if let Some(p) = resolve(a) {
                        self.cache.preload(&p);
                    }
                }
            }
            AudioAction::StopAll { fade } => {
                for w in self.waiting.drain(..) {
                    self.voices.remove(&w.cue);
                }
                self.seeks.clear();
                self.streamed.clear();
                let fade_frames = self.frames(*fade);
                self.send(RtCommand::StopAll { fade_frames });
            }
            AudioAction::SetMasterGain { db } => {
                self.send(RtCommand::SetMasterGain { gain: db_to_gain(*db), fade_frames: 480 })
            }
        }
    }

    /// Starts a play once its clips are loaded (failed ones are skipped) and
    /// its disk streams are primed; returns false, starting nothing, before.
    fn try_start(&mut self, w: &mut WaitingPlay) -> bool {
        let AudioAction::Play { cue, tracks, click, gain_db, fade_in, start_at, end_at, looped } = &w.play else {
            unreachable!("waiting plays hold Play actions")
        };
        let start_frame = start_at.to_frames(self.sample_rate);
        let end_frame = end_at.map(|e| e.to_frames(self.sample_rate));
        if w.voices.is_none() {
            let mut clips = Vec::with_capacity(w.paths.len());
            for p in &w.paths {
                clips.push(match p {
                    Some(p) => match self.cache.poll(p) {
                        Some(c) => Some(c),
                        None => return false,
                    },
                    None => None,
                });
            }
            let region = (start_frame, end_frame.unwrap_or(u64::MAX));
            let mut voices = Vec::with_capacity(tracks.len());
            for (t, clip) in tracks.iter().zip(clips) {
                let source = match clip {
                    Some(Ok(Clip::Resident(c))) => TrackSource::Resident(c),
                    Some(Ok(Clip::Streamed(c))) => match open_stream(&c, start_frame, region, *looped, &self.shared) {
                        Ok((reader, job)) => {
                            self.streamer.submit(job);
                            TrackSource::Streamed(reader)
                        }
                        Err(e) => {
                            tracing::error!(asset = %t.asset, "cannot stream audio: {e}");
                            continue;
                        }
                    },
                    Some(Err(e)) => {
                        tracing::error!(asset = %t.asset, "audio unavailable: {e}");
                        continue;
                    }
                    None => continue,
                };
                voices.push(TrackVoice { source, stem: t.stem, gain: db_to_gain(t.gain_db) });
            }
            w.voices = Some(voices);
        }
        let primed = |v: &TrackVoice| !matches!(&v.source, TrackSource::Streamed(r) if !r.primed());
        if !w.voices.as_ref().is_some_and(|v| v.iter().all(primed)) {
            return false;
        }
        let voices = w.voices.take().unwrap_or_default();
        let streams: Vec<Option<Arc<StreamedClip>>> = voices
            .iter()
            .map(|v| match &v.source {
                TrackSource::Streamed(r) => Some(r.clip().clone()),
                TrackSource::Resident(_) => None,
            })
            .collect();
        if streams.iter().any(Option::is_some) {
            let longest = voices.iter().map(|v| v.source.frames()).max().unwrap_or(0);
            let play_len = end_frame.map_or(longest, |e| e.min(longest)).saturating_sub(start_frame);
            let play = StreamedPlay { shared: w.shared.clone(), tracks: streams, start_frame, end_frame, looped: *looped, play_len };
            self.streamed.insert(*cue, play);
        }
        let click = click.as_ref().map(|c| (Metronome::new(c, self.sample_rate), c.stem));
        let count_in = match (click.as_ref(), &w.play) {
            (Some((m, _)), AudioAction::Play { click: Some(c), start_at, .. }) if start_at.is_zero() => m.count_in_frames(c.count_in_bars),
            _ => 0,
        };
        let level = db_to_gain(*gain_db);
        let mut gain = Ramp::new(if fade_in.is_zero() { level } else { 0.0 });
        if !fade_in.is_zero() {
            gain.to(level, self.frames(*fade_in));
        }
        let mut group = Box::new(VoiceGroup {
            cue: *cue,
            tracks: voices,
            click,
            count_in,
            frame: 0,
            start_frame,
            end_frame,
            looped: *looped,
            gain,
            level,
            state: GroupState::Playing,
            shared: w.shared.clone(),
            pending_seek: None,
            pending_streams: None,
            retired: None,
        });
        // Keep the click phase-aligned with the song position when starting mid-way.
        if let Some((m, _)) = group.click.as_mut() {
            m.seek(start_frame);
        }
        self.send(RtCommand::Start(group));
        let none = |_: &AssetRef| None;
        for a in &w.deferred {
            self.handle(a, &none);
        }
        true
    }

    /// Queues a transport change for a cue still waiting on its clips, or
    /// drops the wait on stop. Returns whether the action was consumed.
    fn defer_for_waiting(&mut self, action: &AudioAction) -> bool {
        let cue = match action {
            AudioAction::Stop { cue, .. }
            | AudioAction::Pause { cue }
            | AudioAction::Resume { cue }
            | AudioAction::Seek { cue, .. }
            | AudioAction::SetGain { cue, .. } => *cue,
            _ => return false,
        };
        let Some(i) = self.waiting.iter().position(|w| w.cue == cue) else { return false };
        if matches!(action, AudioAction::Stop { .. }) {
            self.waiting.remove(i);
            self.voices.remove(&cue);
            self.streamed.remove(&cue);
        } else {
            self.waiting[i].deferred.push(action.clone());
        }
        true
    }

    /// Sends seeks whose new streams have primed.
    fn poll_seeks(&mut self) {
        let mut i = 0;
        while i < self.seeks.len() {
            if self.seeks[i].streams.iter().flatten().all(StreamReader::primed) {
                let WaitingSeek { cue, position, streams } = self.seeks.remove(i);
                self.send(RtCommand::SeekStreams { cue, position, streams: Box::new(streams) });
            } else {
                i += 1;
            }
        }
    }

    /// Times a disk stream ran dry during playback (heard as a dropout).
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Sample-clock position of a cue (drives timeline children).
    pub fn media_position(&self, cue: CueId) -> Option<ShowTime> {
        self.voice_clock(cue).map(|c| c.read().position)
    }

    /// Lock-free handle on a cue's sample clock for other threads (the video
    /// output thread frame-locks timeline layers to it).
    pub fn voice_clock(&self, cue: CueId) -> Option<VoiceClock> {
        let voice = self.voices.get(&cue)?.clone();
        Some(VoiceClock { voice, engine: self.shared.clone(), sample_rate: self.sample_rate })
    }

    /// The device sample clock: frames rendered since the engine started,
    /// interpolated between callbacks. None until the device has called back.
    pub fn sample_clock(&self) -> Option<ShowTime> {
        let extra = self.shared.frames_since_callback(self.sample_rate)?;
        let frames = self.shared.frames_rendered.load(Ordering::Relaxed) + extra;
        Some(ShowTime::from_frames(frames, self.sample_rate))
    }

    pub fn media_finished(&self, cue: CueId) -> bool {
        self.voices.get(&cue).is_some_and(|v| v.finished.load(Ordering::Acquire))
    }

    /// Drops objects returned by the audio thread. Call every UI frame.
    pub fn maintenance(&mut self) {
        if !self.waiting.is_empty() {
            for mut w in std::mem::take(&mut self.waiting) {
                if !self.try_start(&mut w) {
                    self.waiting.push(w);
                }
            }
        }
        if !self.seeks.is_empty() {
            self.poll_seeks();
        }
        let underruns = self.underruns();
        if underruns > self.underruns_logged {
            tracing::warn!(total = underruns, "disk stream underrun: a long file played silence (drive too slow?)");
            self.underruns_logged = underruns;
        }
        let mut n = 0;
        while let Some(g) = self.garbage.try_pop() {
            if let Garbage::Group(g) = &g {
                // Forget status of groups nobody will query any more.
                if self.streamed.get(&g.cue).is_some_and(|p| Arc::ptr_eq(&p.shared, &g.shared)) {
                    self.streamed.remove(&g.cue);
                }
                if let Some(v) = self.voices.get(&g.cue) {
                    if Arc::ptr_eq(v, &g.shared) && !g.shared.finished.load(Ordering::Acquire) {
                        // Stopped by the operator (not natural end): forget it.
                        self.voices.remove(&g.cue);
                    }
                }
            }
            drop(g);
            n += 1;
        }
        if n > 0 {
            tracing::trace!(n, "audio garbage collected");
        }
    }

    /// Reads and resets peak levels for all outputs.
    pub fn take_peaks(&self) -> Vec<f32> {
        (0..self.channels as usize).map(|c| self.shared.take_peak(c)).collect()
    }

    pub fn dsp_load(&self) -> f32 {
        self.shared.load_permille.load(Ordering::Relaxed) as f32 / 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestage_core::action::{AudioTrack, ClickTrack};

    fn sine(rate: u32, channels: u16, secs: f32) -> AudioClip {
        let frames = (rate as f32 * secs) as usize;
        let mut data = Vec::with_capacity(frames * channels as usize);
        for f in 0..frames {
            let s = (f as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
            for _ in 0..channels {
                data.push(s);
            }
        }
        AudioClip { sample_rate: rate, channels, data }
    }

    fn energy(buf: &[f32], ch: usize, out: usize) -> f32 {
        buf.chunks(ch).map(|f| f[out].abs()).sum()
    }

    #[test]
    fn routes_stems_to_patched_outputs_only() {
        let routing = AudioRouting::default();
        let (mut engine, mut rt) = AudioEngine::offline(48_000, 8, &routing);
        engine.insert_clip("/song.wav", sine(48_000, 2, 1.0));
        let backing = routing.stem_by_name("Backing Tracks").unwrap().id;
        let click = routing.stem_by_name("Click").unwrap().id;
        let cue = CueId::new();
        engine.handle(
            &AudioAction::Play {
                cue,
                tracks: vec![AudioTrack { asset: AssetRef::new("song.wav"), stem: backing, gain_db: 0.0, mute: false }],
                click: Some(ClickTrack { bpm: 120.0, beats_per_bar: 4, stem: click, gain_db: 0.0, count_in_bars: 0 }),
                gain_db: 0.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                end_at: None,
                looped: false,
            },
            &|a| Some(PathBuf::from("/").join(a.as_str())),
        );
        let mut buf = vec![0.0f32; 512 * 8];
        rt.render(&mut buf);
        for out in 0..2 {
            assert!(energy(&buf, 8, out) > 1.0, "FOH {out} silent");
        }
        for out in 2..4 {
            assert!(energy(&buf, 8, out) > 0.1, "click {out} silent");
        }
        for out in 4..8 {
            assert_eq!(energy(&buf, 8, out), 0.0, "IEM {out} should be silent");
        }
        assert_eq!(engine.media_position(cue), Some(ShowTime::from_frames(511, 48_000)));
    }

    #[test]
    fn stop_fades_then_returns_garbage() {
        let routing = AudioRouting::default();
        let (mut engine, mut rt) = AudioEngine::offline(48_000, 8, &routing);
        engine.insert_clip("/a.wav", sine(48_000, 2, 10.0));
        let stem = routing.stems[0].id;
        let cue = CueId::new();
        let play = AudioAction::Play {
            cue,
            tracks: vec![AudioTrack { asset: AssetRef::new("a.wav"), stem, gain_db: 0.0, mute: false }],
            click: None,
            gain_db: 0.0,
            fade_in: ShowTime::ZERO,
            start_at: ShowTime::ZERO,
            end_at: None,
            looped: false,
        };
        let resolve = |a: &AssetRef| Some(PathBuf::from("/").join(a.as_str()));
        engine.handle(&play, &resolve);
        let mut buf = vec![0.0f32; 256 * 8];
        rt.render(&mut buf);
        engine.handle(&AudioAction::Stop { cue, fade: ShowTime::from_millis(10) }, &resolve);
        for _ in 0..4 {
            rt.render(&mut buf); // 4 * 256 > 480 fade frames
        }
        assert_eq!(energy(&buf, 8, 0), 0.0);
        assert_eq!(engine.shared().active_groups.load(Ordering::Relaxed), 0);
        engine.maintenance();
        assert!(engine.media_position(cue).is_none(), "stopped voice status released");
    }

    #[test]
    fn natural_end_reports_finished() {
        let routing = AudioRouting::default();
        let (mut engine, mut rt) = AudioEngine::offline(48_000, 2, &routing);
        engine.insert_clip("/short.wav", sine(48_000, 1, 0.01)); // 480 frames
        let cue = CueId::new();
        engine.handle(
            &AudioAction::Play {
                cue,
                tracks: vec![AudioTrack { asset: AssetRef::new("short.wav"), stem: routing.stems[0].id, gain_db: 0.0, mute: false }],
                click: None,
                gain_db: 0.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                end_at: None,
                looped: false,
            },
            &|a| Some(PathBuf::from("/").join(a.as_str())),
        );
        let mut buf = vec![0.0f32; 1024 * 2];
        rt.render(&mut buf);
        assert!(engine.media_finished(cue));
    }

    #[test]
    fn live_seek_jumps_without_a_click() {
        let routing = AudioRouting::default();
        let (mut engine, mut rt) = AudioEngine::offline(48_000, 2, &routing);
        // Mono ramp: the sample value encodes the position, so a jump is a step.
        let frames = 96_000;
        let data = (0..frames).map(|f| 0.5 * f as f32 / frames as f32).collect();
        engine.insert_clip("/ramp.wav", AudioClip { sample_rate: 48_000, channels: 1, data });
        let stem = routing.stems[0].id;
        let cue = CueId::new();
        let resolve = |a: &AssetRef| Some(PathBuf::from("/").join(a.as_str()));
        engine.handle(
            &AudioAction::Play {
                cue,
                tracks: vec![AudioTrack { asset: AssetRef::new("ramp.wav"), stem, gain_db: 0.0, mute: false }],
                click: None,
                gain_db: 0.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                end_at: None,
                looped: false,
            },
            &resolve,
        );
        let mut before = vec![0.0f32; 256 * 2];
        rt.render(&mut before);
        engine.handle(&AudioAction::Seek { cue, position: ShowTime::from_secs(1) }, &resolve);
        // The clock follows the seek immediately, before the audio thread runs.
        assert_eq!(engine.media_position(cue), Some(ShowTime::from_secs(1)));
        let mut after = vec![0.0f32; 512 * 2];
        rt.render(&mut after);
        let left: Vec<f32> = before.iter().chain(after.iter()).step_by(2).copied().collect();
        let worst = left.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(worst < 0.01, "audible step at the splice: {worst}");
        // Playing from 1 s: the ramp is at ~0.25 once the dip is over.
        let last = *left.last().unwrap();
        assert!((last - 0.25).abs() < 0.01, "not playing from the new position: {last}");
        let pos = engine.media_position(cue).unwrap();
        assert!(pos > ShowTime::from_secs(1) && pos < ShowTime::from_millis(1_020), "{pos:?}");
    }

    /// Streamed playback is sample-identical to resident playback: through
    /// the pre-roll hand-over, a seek past the pre-roll and a loop.
    #[test]
    fn streamed_playback_matches_resident() {
        let dir = std::env::temp_dir().join(format!("ls-streamplay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let n = 48_000 * 20;
        let samples: Vec<i16> = (0..n).flat_map(|i| [((i % 30_000) as i16), -((i % 17_000) as i16)]).collect();
        let path = dir.join("song.wav");
        crate::decode::tests::write_wav(&path, 48_000, 2, &samples);
        let routing = AudioRouting::default();
        let (mut streamed, mut s_rt) = AudioEngine::offline(48_000, 2, &routing);
        let (mut resident, mut r_rt) = AudioEngine::offline(48_000, 2, &routing);
        resident.insert_clip(path.clone(), crate::decode::decode_file(&path).unwrap());
        let wait = |e: &mut AudioEngine, done: &dyn Fn(&AudioEngine) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !done(e) {
                assert!(std::time::Instant::now() < deadline, "timed out");
                e.maintenance();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        let resolve = |a: &AssetRef| Some(dir.join(a.as_str()));
        streamed.handle(&AudioAction::Preload { assets: vec![AssetRef::new("song.wav")] }, &resolve);
        wait(&mut streamed, &|e| matches!(e.cache.poll(&path), Some(Ok(Clip::Streamed(_)))));

        for (looped, end_at) in [(false, None), (true, Some(ShowTime::from_secs(9)))] {
            let cue = CueId::new();
            let play = AudioAction::Play {
                cue,
                tracks: vec![AudioTrack { asset: AssetRef::new("song.wav"), stem: routing.stems[0].id, gain_db: 0.0, mute: false }],
                click: None,
                gain_db: 0.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::from_millis(500),
                end_at,
                looped,
            };
            streamed.handle(&play, &resolve);
            resident.handle(&play, &resolve);
            assert!(streamed.streamed.contains_key(&cue), "starts at once from the pre-roll");
            let (mut a, mut b) = (vec![0.0f32; 256 * 2], vec![0.0f32; 256 * 2]);
            let mut loudest = 0.0f32;
            let mut compare = |s_rt: &mut Renderer, r_rt: &mut Renderer, buffers: usize, what: &str| {
                for i in 0..buffers {
                    s_rt.render(&mut a);
                    r_rt.render(&mut b);
                    assert_eq!(a, b, "{what}: buffer {i} differs");
                    loudest = a.iter().fold(loudest, |m, s| m.max(s.abs()));
                    // Pace the "device" at a few times real time so the streamer keeps up.
                    if i % 4 == 0 {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            };
            // 6 s: pre-roll (3 s) then the ring.
            compare(&mut s_rt, &mut r_rt, 1_125, "playback");
            // Seek far past the pre-roll; the resident engine seeks when the streams are primed.
            let target = ShowTime::from_secs(if looped { 7 } else { 12 });
            let seek = AudioAction::Seek { cue, position: target };
            streamed.handle(&seek, &resolve);
            // While the streams prime, the clock already reports the target (held).
            assert_eq!(streamed.voice_clock(cue).unwrap().read(), ClockReading { position: target, running: false, ended: false });
            wait(&mut streamed, &|e| e.seeks.is_empty());
            resident.handle(&seek, &resolve);
            // 4 s more: across the loop point for the looped pass.
            compare(&mut s_rt, &mut r_rt, 750, "after seek");
            assert_eq!(streamed.media_position(cue), resident.media_position(cue));
            for e in [&mut streamed, &mut resident] {
                e.handle(&AudioAction::Stop { cue, fade: ShowTime::ZERO }, &resolve);
            }
            compare(&mut s_rt, &mut r_rt, 10, "stop");
            assert!(loudest > 0.2, "silent comparison proves nothing ({loudest})");
        }
        assert_eq!(streamed.underruns(), 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn go_before_decode_starts_later_without_blocking() {
        let dir = std::env::temp_dir().join(format!("ls-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let samples: Vec<i16> = (0..48_000).map(|i| ((i % 100) * 300) as i16).collect();
        crate::decode::tests::write_wav(&dir.join("late.wav"), 48_000, 1, &samples);
        let routing = AudioRouting::default();
        let (mut engine, mut rt) = AudioEngine::offline(48_000, 2, &routing);
        let resolve = |a: &AssetRef| Some(dir.join(a.as_str()));
        let play = |cue| AudioAction::Play {
            cue,
            tracks: vec![AudioTrack { asset: AssetRef::new("late.wav"), stem: routing.stems[0].id, gain_db: 0.0, mute: false }],
            click: None,
            gain_db: 0.0,
            fade_in: ShowTime::ZERO,
            start_at: ShowTime::ZERO,
            end_at: None,
            looped: false,
        };

        // Stopped before the decode lands: never starts.
        let dropped = CueId::new();
        engine.handle(&play(dropped), &resolve);
        engine.handle(&AudioAction::Stop { cue: dropped, fade: ShowTime::ZERO }, &resolve);
        assert_eq!(engine.media_position(dropped), None);

        let cue = CueId::new();
        engine.handle(&play(cue), &resolve);
        // The clock exists but holds at zero while the loader decodes.
        assert_eq!(engine.voice_clock(cue).unwrap().read(), ClockReading { position: ShowTime::ZERO, running: false, ended: false });
        engine.handle(&AudioAction::SetGain { cue, gain_db: -6.0, fade: ShowTime::ZERO }, &resolve);
        let mut buf = vec![0.0f32; 256 * 2];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while engine.media_position(cue) == Some(ShowTime::ZERO) {
            assert!(std::time::Instant::now() < deadline, "never started");
            engine.maintenance();
            rt.render(&mut buf);
            std::thread::yield_now();
        }
        assert!(buf.iter().any(|s| s.abs() > 0.01), "silent after start");
        assert_eq!(engine.media_position(dropped), None);
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod send_tests {
    #[test]
    fn engine_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<super::AudioEngine>();
    }
}
