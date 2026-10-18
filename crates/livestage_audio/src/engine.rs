//! Control-thread side of the audio engine.

use crate::clip::AudioClip;
use crate::decode::decode_file;
use crate::device::{build_stream, select_device};
use crate::metronome::Metronome;
use crate::renderer::*;
use crate::routing::RoutingTable;
use crate::{db_to_gain, AudioResult};
use cpal::traits::StreamTrait;
use livestage_core::action::AudioAction;
use livestage_core::id::CueId;
use livestage_core::project::AssetRef;
use livestage_core::routing::AudioRouting;
use livestage_core::time::ShowTime;
use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
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

#[derive(Debug, Clone)]
enum ClipSlot {
    Loading,
    Ready(Arc<AudioClip>),
    Failed(String),
}

/// Decoded-clip cache with a background loader thread.
///
/// The mutex here is only ever touched by control/loader threads — never by
/// the audio thread, which receives `Arc<AudioClip>`s inside commands.
#[derive(Clone)]
struct ClipCache {
    slots: Arc<Mutex<HashMap<PathBuf, ClipSlot>>>,
    loader: mpsc::Sender<PathBuf>,
    sample_rate: u32,
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
        Self { slots, loader: tx, sample_rate }
    }

    fn preload(&self, path: &Path) {
        let mut slots = self.slots.lock();
        if !slots.contains_key(path) {
            slots.insert(path.to_path_buf(), ClipSlot::Loading);
            let _ = self.loader.send(path.to_path_buf());
        }
    }

    /// Returns the clip, decoding synchronously if it wasn't preloaded.
    fn get_blocking(&self, path: &Path) -> Result<Arc<AudioClip>, String> {
        let current = self.slots.lock().get(path).cloned();
        match current {
            Some(ClipSlot::Ready(c)) => Ok(c),
            Some(ClipSlot::Failed(e)) => Err(e),
            // Not loaded or still loading: decode here. GO must never be lost.
            _ => {
                tracing::warn!(file = %path.display(), "asset not preloaded; decoding on GO");
                let slot = load(path, self.sample_rate);
                self.slots.lock().insert(path.to_path_buf(), slot.clone());
                match slot {
                    ClipSlot::Ready(c) => Ok(c),
                    ClipSlot::Failed(e) => Err(e),
                    ClipSlot::Loading => unreachable!(),
                }
            }
        }
    }

    fn insert(&self, path: PathBuf, clip: Arc<AudioClip>) {
        self.slots.lock().insert(path, ClipSlot::Ready(clip));
    }
}

fn load(path: &Path, sample_rate: u32) -> ClipSlot {
    match decode_file(path) {
        Ok(clip) => {
            let clip = clip.resampled(sample_rate);
            tracing::info!(file = %path.display(), secs = clip.duration_secs(), "audio loaded");
            ClipSlot::Ready(Arc::new(clip))
        }
        Err(e) => {
            tracing::error!("{e}");
            ClipSlot::Failed(e.to_string())
        }
    }
}

/// Owner of the audio pipeline. Lives on the control thread.
pub struct AudioEngine {
    tx: HeapProd<RtCommand>,
    garbage: HeapCons<Garbage>,
    shared: Arc<EngineShared>,
    voices: HashMap<CueId, Arc<VoiceShared>>,
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
        let renderer = Renderer::new(rx, gtx, table, channels as usize, sample_rate, shared.clone());
        let engine = Self {
            tx,
            garbage: grx,
            shared,
            voices: HashMap::new(),
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

    /// Crosspoints currently ignored because the device has too few outputs.
    pub fn dropped_routes(&self) -> &[(String, u16)] {
        &self.dropped_routes
    }

    /// Makes a clip available under `path` without decoding (tests, generated audio).
    pub fn insert_clip(&self, path: impl Into<PathBuf>, clip: AudioClip) {
        self.cache.insert(path.into(), Arc::new(clip.resampled(self.sample_rate)));
    }

    pub fn preload(&self, path: &Path) {
        self.cache.preload(path);
    }

    /// Executes a core [`AudioAction`]. `resolve` maps bundle asset refs to
    /// absolute paths.
    pub fn handle(&mut self, action: &AudioAction, resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>) {
        match action {
            AudioAction::Play { cue, tracks, click, gain_db, fade_in, start_at, end_at, looped } => {
                let mut voices = Vec::with_capacity(tracks.len());
                for t in tracks.iter().filter(|t| !t.mute) {
                    let Some(path) = resolve(&t.asset) else {
                        tracing::error!(asset = %t.asset, "cannot resolve audio asset");
                        continue;
                    };
                    match self.cache.get_blocking(&path) {
                        Ok(clip) => voices.push(TrackVoice { clip, stem: t.stem, gain: db_to_gain(t.gain_db) }),
                        Err(e) => tracing::error!(asset = %t.asset, "audio unavailable: {e}"),
                    }
                }
                let click = click.as_ref().map(|c| (Metronome::new(c, self.sample_rate), c.stem));
                let count_in = match (click.as_ref(), action) {
                    (Some((m, _)), AudioAction::Play { click: Some(c), start_at, .. }) if start_at.is_zero() => {
                        m.count_in_frames(c.count_in_bars)
                    }
                    _ => 0,
                };
                let level = db_to_gain(*gain_db);
                let mut gain = Ramp::new(if fade_in.is_zero() { level } else { 0.0 });
                if !fade_in.is_zero() {
                    gain.to(level, self.frames(*fade_in));
                }
                let shared = Arc::new(VoiceShared::default());
                self.voices.insert(*cue, shared.clone());
                let start_frame = start_at.to_frames(self.sample_rate);
                let mut group = Box::new(VoiceGroup {
                    cue: *cue,
                    tracks: voices,
                    click,
                    count_in,
                    frame: 0,
                    start_frame,
                    end_frame: end_at.map(|e| e.to_frames(self.sample_rate)),
                    looped: *looped,
                    gain,
                    level,
                    state: GroupState::Playing,
                    shared,
                    pending_seek: None,
                });
                // Keep the click phase-aligned with the song position when starting mid-way.
                if let Some((m, _)) = group.click.as_mut() {
                    m.seek(start_frame);
                }
                self.send(RtCommand::Start(group));
            }
            AudioAction::Stop { cue, fade } => {
                let fade_frames = self.frames(*fade);
                self.send(RtCommand::Stop { cue: *cue, fade_frames });
            }
            AudioAction::Pause { cue } => self.send(RtCommand::Pause { cue: *cue }),
            AudioAction::Resume { cue } => self.send(RtCommand::Resume { cue: *cue }),
            AudioAction::Seek { cue, position } => {
                let position = position.to_frames(self.sample_rate);
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
                let fade_frames = self.frames(*fade);
                self.send(RtCommand::StopAll { fade_frames });
            }
            AudioAction::SetMasterGain { db } => {
                self.send(RtCommand::SetMasterGain { gain: db_to_gain(*db), fade_frames: 480 })
            }
        }
    }

    /// Sample-clock position of a cue (drives timeline children).
    pub fn media_position(&self, cue: CueId) -> Option<ShowTime> {
        let v = self.voices.get(&cue)?;
        if !v.started.load(Ordering::Acquire) {
            return Some(ShowTime::ZERO);
        }
        let frames = v.position_frames.load(Ordering::Relaxed) + self.interpolated_frames(v);
        Some(ShowTime::from_frames(frames, self.sample_rate))
    }

    /// Frames elapsed since the last device callback, capped at one buffer.
    /// The sample clock only advances once per callback (e.g. every 5.3 ms at
    /// 256 frames); interpolating removes that step from timeline scheduling.
    fn interpolated_frames(&self, v: &VoiceShared) -> u64 {
        let at = self.shared.callback_at_ns.load(Ordering::Acquire);
        if at == 0 || !v.playing.load(Ordering::Relaxed) {
            return 0;
        }
        let since_ns = (self.shared.epoch.elapsed().as_nanos() as u64).saturating_sub(at);
        let frames = since_ns * self.sample_rate as u64 / 1_000_000_000;
        frames.min(self.shared.callback_frames.load(Ordering::Relaxed) as u64)
    }

    pub fn media_finished(&self, cue: CueId) -> bool {
        self.voices.get(&cue).is_some_and(|v| v.finished.load(Ordering::Acquire))
    }

    /// Drops objects returned by the audio thread. Call every UI frame.
    pub fn maintenance(&mut self) {
        let mut n = 0;
        while let Some(g) = self.garbage.try_pop() {
            if let Garbage::Group(g) = &g {
                // Forget status of groups nobody will query any more.
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

    pub fn pending_commands(&self) -> usize {
        self.tx.occupied_len()
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
}

#[cfg(test)]
mod send_tests {
    #[test]
    fn engine_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<super::AudioEngine>();
    }
}
