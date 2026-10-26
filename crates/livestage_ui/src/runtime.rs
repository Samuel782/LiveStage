//! Show runtime: owns the project bundle and every engine, executes
//! [`ShowCommand`]s and routes cue [`EngineAction`]s to engines.

use livestage_audio::{AudioEngine, EngineConfig};
use livestage_control::midi::{MidiInputHandler, MidiOutputHandler};
use livestage_control::osc::{OscSender, OscServer};
use livestage_control::timecode::{ArtNetTimecodeListener, MtcInput, MtcOutput, TimecodeInput};
use livestage_control::websocket::RemoteServer;
use livestage_control::{ControlEvent, ControlHub, ControlSource, PanicController};
use livestage_core::action::{AudioAction, ControlAction, EngineAction};
use livestage_core::cue::{AnyCue, Cue, CueContext, CueExt};
use livestage_core::cart::CueCart;
use livestage_core::cuelist::{CueList, CueListStatus};
use livestage_core::id::{CueId, CueListId};
use livestage_core::project::{AssetKind, AssetRef};
use livestage_core::timecode::{TimecodeEngine, TimecodeSource};
use livestage_core::{ProjectBundle, ShowCommand, ShowTime};
use livestage_dmx::DmxEngine;
use livestage_core::action::VideoAction;
use parking_lot::Mutex;
use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Video actions stamped with the show time they were dispatched at, sent to
/// the render thread which owns the compositor.
pub type VideoMsg = (ShowTime, VideoAction);

/// Cues whose video media has ended, published by the render thread.
pub type VideoFinished = Arc<Mutex<HashSet<CueId>>>;

/// Slide navigation requested by the operator or a control surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlideNav {
    Next,
    Previous,
    /// 0-based.
    Goto(usize),
}

/// Operator action on a single cue (context menus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueOp {
    Pause,
    Resume,
    /// Stop with the cue's own fade-out.
    Stop,
}

/// A presentation being rendered to slides on a background thread.
pub struct DocImport {
    pub name: String,
    /// (slides done, total) — total is 0 until the document is opened.
    pub progress: Arc<(std::sync::atomic::AtomicUsize, std::sync::atomic::AtomicUsize)>,
    result: crossbeam_channel::Receiver<Result<livestage_core::cue::SlideDeck, String>>,
    dir: std::path::PathBuf,
}

/// Depth-first search for a cue in every list (timeline children included).
/// Gives a copied cue (and its timeline children) fresh ids and idle state.
pub fn renew_ids(c: &mut AnyCue) {
    let h = c.header_mut();
    h.id = CueId::new();
    h.runtime = Default::default();
    if let AnyCue::Timeline(t) = c {
        if let Some(m) = t.master.as_deref_mut() {
            renew_ids(m);
        }
        for i in &mut t.items {
            i.fired = false;
            renew_ids(&mut i.cue);
        }
    }
}

/// Depth-first search in every list and cart (timeline children included).
pub(crate) fn find_cue_in<'a>(lists: &'a mut [CueList], carts: &'a mut [CueCart], id: CueId) -> Option<&'a mut AnyCue> {
    fn find(c: &mut AnyCue, id: CueId) -> Option<&mut AnyCue> {
        if c.header().id == id {
            return Some(c);
        }
        if let AnyCue::Timeline(t) = c {
            if let Some(m) = t.master.as_deref_mut() {
                if let Some(f) = find(m, id) {
                    return Some(f);
                }
            }
            for i in &mut t.items {
                if let Some(f) = find(&mut i.cue, id) {
                    return Some(f);
                }
            }
        }
        None
    }
    lists
        .iter_mut()
        .flat_map(|l| l.cues.iter_mut())
        .chain(carts.iter_mut().flat_map(|c| c.pads.iter_mut().flatten().map(|p| &mut p.cue)))
        .find_map(|c| find(c, id))
}

/// Health counters for the show thread.
#[derive(Debug, Default)]
pub struct ShowStats {
    pub ticks: AtomicU64,
    /// Worst tick duration (µs) since last read.
    pub max_tick_us: AtomicU64,
    /// Worst wait for the runtime lock (µs) since last read.
    pub max_wait_us: AtomicU64,
    /// Worst scheduling lateness (µs) since last read.
    pub max_late_us: AtomicU64,
    /// Scheduling class obtained by the show thread.
    pub scheduling: Mutex<String>,
}

impl ShowStats {
    pub fn record(slot: &AtomicU64, us: u64) {
        slot.fetch_max(us, Ordering::Relaxed);
    }
    pub fn take(slot: &AtomicU64) -> u64 {
        slot.swap(0, Ordering::Relaxed)
    }
}

/// Adapter giving cues access to engine clocks without borrowing the runtime.
struct Ctx<'a> {
    now: ShowTime,
    actions: &'a mut Vec<EngineAction>,
    audio: Option<&'a AudioEngine>,
    video_finished: &'a HashSet<CueId>,
}

impl CueContext for Ctx<'_> {
    fn now(&self) -> ShowTime {
        self.now
    }
    fn dispatch(&mut self, action: EngineAction) {
        self.actions.push(action);
    }
    fn media_position(&self, cue: CueId) -> Option<ShowTime> {
        // Video cues run on the shared show clock; only audio has its own sample clock.
        self.audio.and_then(|a| a.media_position(cue))
    }
    fn media_finished(&self, cue: CueId) -> bool {
        self.audio.is_some_and(|a| a.media_finished(cue)) || self.video_finished.contains(&cue)
    }
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub at: ShowTime,
    pub text: String,
    pub level: LogLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Snapshot of one cue for the grid.
#[derive(Debug, Clone)]
pub struct CueRow {
    pub id: CueId,
    pub depth: usize,
    pub number: String,
    pub name: String,
    pub kind: &'static str,
    pub state: livestage_core::CueState,
    pub pre_wait: ShowTime,
    pub post_wait: ShowTime,
    pub duration: Option<ShowTime>,
    pub elapsed: ShowTime,
    pub progress: Option<f32>,
    pub follow: livestage_core::cue::FollowMode,
    pub armed: bool,
    pub offset: Option<ShowTime>,
    pub is_standby: bool,
    /// Extra status shown in the action column (e.g. "Slide 3 / 12").
    pub detail: Option<String>,
    /// Timecode trigger / slave start, shown in place of the pre-wait.
    pub timecode: Option<livestage_core::Timecode>,
}

pub struct ShowRuntime {
    pub bundle: ProjectBundle,
    pub audio: Option<AudioEngine>,
    pub audio_error: Option<String>,
    pub dmx: DmxEngine,
    video_tx: crossbeam_channel::Sender<VideoMsg>,
    video_rx: crossbeam_channel::Receiver<VideoMsg>,
    pub video_finished: VideoFinished,
    pub show_stats: Arc<ShowStats>,
    pub hub: ControlHub,
    osc: Option<OscServer>,
    remote: Option<RemoteServer>,
    pub midi_in: Option<MidiInputHandler>,
    midi_out: Option<MidiOutputHandler>,
    osc_out: Option<OscSender>,
    panic: PanicController,
    epoch: Instant,
    actions: Vec<EngineAction>,
    pub log: VecDeque<LogLine>,
    pub dirty: bool,
    pub undo: crate::undo::UndoStack,
    /// Increments on every edit (autosave change detection).
    pub edit_generation: u64,
    /// Requested change of program-output kiosk state (handled by the app).
    pub outputs_live_request: Option<bool>,
    pub outputs_live: bool,
    last_status: Instant,
    /// Presentations being rendered in the background.
    pub imports: Vec<DocImport>,
    /// LibreOffice binary for PowerPoint fidelity (from Settings; auto-detected if None).
    pub office_path: Option<std::path::PathBuf>,
    /// Set by Settings: the app restarts OSC / WebSocket / MIDI outside the lock.
    pub restart_control_requested: bool,
    /// Show timecode: source selection, lock and internal generator.
    pub timecode: TimecodeEngine,
    tc_tx: crossbeam_channel::Sender<TimecodeInput>,
    tc_rx: crossbeam_channel::Receiver<TimecodeInput>,
    mtc_in: Option<MtcInput>,
    artnet_tc: Option<ArtNetTimecodeListener>,
    ltc_in: Option<livestage_audio::ltc::LtcInput>,
    /// What the timecode input is doing (port name or error), for the UI.
    pub timecode_input: String,
    mtc_out: Option<MtcOutput>,
    /// When the LTC generator was last pointed at the transport, and at what.
    ltc_sent: Option<(Instant, livestage_audio::ltc::LtcAnchor, u64)>,
    /// What timecode generation is doing, for the UI.
    pub timecode_output: String,
    /// Bumped on every log line (UI copies the log only when it changed).
    pub log_seq: u64,
    /// Bumped when the DMX engine is rebuilt (new patch / library).
    pub dmx_generation: u64,
    /// Revision of `bundle.project.displays`; renderers re-copy the displays
    /// only when it changes. Unique across runtimes (see [`touch_displays`](Self::touch_displays)).
    pub displays_rev: u64,
}

/// Source of display revisions, global so a newly opened show never reuses
/// the revision a renderer cached for the previous one.
static DISPLAYS_REV: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl ShowRuntime {
    pub fn new(bundle: ProjectBundle) -> Self {
        let (video_tx, video_rx) = crossbeam_channel::unbounded();
        let (tc_tx, tc_rx) = crossbeam_channel::unbounded();
        let mut dmx = DmxEngine::new(bundle.project.dmx.clone());
        dmx.start_output();
        let mut rt = Self {
            audio: None,
            audio_error: None,
            dmx,
            video_tx,
            video_rx,
            video_finished: Default::default(),
            show_stats: Default::default(),
            hub: ControlHub::default(),
            osc: None,
            remote: None,
            midi_in: None,
            midi_out: None,
            osc_out: OscSender::new().ok(),
            panic: PanicController::default(),
            epoch: Instant::now(),
            actions: Vec::new(),
            log: VecDeque::new(),
            dirty: false,
            undo: crate::undo::UndoStack::new(&bundle.project),
            edit_generation: 0,
            outputs_live_request: None,
            outputs_live: false,
            last_status: Instant::now(),
            imports: Vec::new(),
            office_path: None,
            restart_control_requested: false,
            timecode: TimecodeEngine::new(bundle.project.timecode.clone()),
            tc_tx,
            tc_rx,
            mtc_in: None,
            artnet_tc: None,
            ltc_in: None,
            timecode_input: String::new(),
            mtc_out: None,
            ltc_sent: None,
            timecode_output: String::new(),
            log_seq: 0,
            dmx_generation: 0,
            displays_rev: DISPLAYS_REV.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            bundle,
        };
        for e in rt.dmx.patch_errors.clone() {
            rt.log(LogLevel::Warn, format!("DMX patch: {e}"));
        }
        rt.start_audio();
        rt.start_control();
        rt.start_timecode_input();
        rt.start_timecode_output();
        for a in rt.bundle.missing_assets() {
            rt.log(LogLevel::Warn, format!("missing asset: {a}"));
        }
        for issue in rt.bundle.project.validate() {
            rt.log(LogLevel::Warn, issue);
        }
        rt.reset_lists();
        rt
    }

    pub fn now(&self) -> ShowTime {
        self.epoch.elapsed().into()
    }

    /// Marks `bundle.project.displays` as changed for cached copies.
    pub fn touch_displays(&mut self) {
        self.displays_rev = DISPLAYS_REV.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// The show clock origin (shared with the render thread).
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Receiver for video actions; drained by the render thread.
    pub fn video_receiver(&self) -> crossbeam_channel::Receiver<VideoMsg> {
        self.video_rx.clone()
    }

    /// Sender for video actions (the UI's frame copy sends test patterns through it).
    pub fn video_sender(&self) -> crossbeam_channel::Sender<VideoMsg> {
        self.video_tx.clone()
    }

    pub fn log(&mut self, level: LogLevel, text: impl Into<String>) {
        let text = text.into();
        match level {
            LogLevel::Info => tracing::info!("{text}"),
            LogLevel::Warn => tracing::warn!("{text}"),
            LogLevel::Error => tracing::error!("{text}"),
        }
        self.note(level, text);
    }

    /// Adds a line to the operator log only (the caller already traced it).
    pub fn note(&mut self, level: LogLevel, text: String) {
        self.log.push_back(LogLine { at: self.now(), text, level });
        self.log_seq += 1;
        while self.log.len() > 500 {
            self.log.pop_front();
        }
    }

    // ------------------------------------------------------------ engines --

    pub fn start_audio(&mut self) {
        self.audio = None;
        let routing = &self.bundle.project.audio;
        match AudioEngine::start(&EngineConfig::from_routing(routing), routing) {
            Ok(engine) => {
                let msg = format!(
                    "audio: {} @ {} Hz, {} outputs",
                    engine.device_name(),
                    engine.sample_rate(),
                    engine.channels()
                );
                // The engine compiled the patch at start; only the master level is left to apply.
                let mut engine = engine;
                let db = routing.master_gain_db;
                engine.handle(&AudioAction::SetMasterGain { db }, &|_| None);
                self.audio = Some(engine);
                self.audio_error = None;
                self.log(LogLevel::Info, msg);
            }
            Err(e) => {
                self.audio_error = Some(e.to_string());
                self.log(LogLevel::Error, format!("audio engine offline: {e}"));
            }
        }
    }

    pub fn apply_audio_routing(&mut self) {
        if let Some(a) = &mut self.audio {
            a.set_routing(&self.bundle.project.audio);
        }
    }

    /// Detaches the running control servers so the caller can drop them
    /// (which joins their threads) without holding the runtime lock.
    pub fn take_control_servers(&mut self) -> Vec<Box<dyn std::any::Any + Send>> {
        let mut old: Vec<Box<dyn std::any::Any + Send>> = Vec::new();
        if let Some(s) = self.osc.take() {
            old.push(Box::new(s));
        }
        if let Some(s) = self.remote.take() {
            old.push(Box::new(s));
        }
        if let Some(m) = self.midi_in.take() {
            old.push(Box::new(m));
        }
        old
    }

    /// Starts OSC / WebSocket / MIDI from the show's control settings.
    pub fn start_control_services(&mut self) {
        self.start_control();
        let osc = self.osc_port().map_or("off".into(), |p| p.to_string());
        let ws = self.ws_port().map_or("off".into(), |p| p.to_string());
        self.log(LogLevel::Info, format!("control services restarted (OSC {osc}, WebSocket {ws})"));
    }

    fn start_control(&mut self) {
        let cfg = self.bundle.project.control.clone();
        if cfg.osc_enabled {
            match OscServer::start(cfg.osc_listen_port, self.hub.sender()) {
                Ok(s) => self.osc = Some(s),
                Err(e) => self.log(LogLevel::Error, format!("OSC port {}: {e}", cfg.osc_listen_port)),
            }
        }
        if cfg.websocket_enabled {
            match RemoteServer::start(cfg.websocket_port, self.hub.sender()) {
                Ok(s) => self.remote = Some(s),
                Err(e) => self.log(LogLevel::Error, format!("WebSocket port {}: {e}", cfg.websocket_port)),
            }
        }
        self.open_midi(cfg.midi_input.as_deref());
    }

    pub fn open_midi(&mut self, port: Option<&str>) {
        self.midi_in = None;
        let id = self.bundle.project.control.msc_device_id;
        match MidiInputHandler::open(port, self.midi_bindings(), id, self.hub.sender()) {
            Ok(h) => {
                let msg = format!("MIDI input: {}", h.port_name);
                self.midi_in = Some(h);
                self.log(LogLevel::Info, msg);
            }
            Err(e) => tracing::info!("MIDI input not opened: {e}"),
        }
    }

    /// Applies edited timecode settings (source, rate, ports, lock tuning).
    pub fn apply_timecode_config(&mut self) {
        let cfg = self.bundle.project.timecode.clone();
        let old = self.timecode.config();
        let reopen = cfg.source != old.source
            || cfg.mtc_input != old.mtc_input
            || cfg.artnet_port != old.artnet_port
            || cfg.ltc_input != old.ltc_input
            || cfg.ltc_channel != old.ltc_channel;
        let reopen_out = cfg.output.mtc != old.output.mtc || cfg.output.mtc_port != old.output.mtc_port || cfg.output.ltc != old.output.ltc;
        self.timecode.set_config(cfg);
        if reopen {
            self.start_timecode_input();
        }
        if reopen_out {
            self.start_timecode_output();
        }
    }

    /// Opens the MTC output and describes the generators (LTC out runs in
    /// the audio engine and needs no opening).
    pub fn start_timecode_output(&mut self) {
        let out = self.bundle.project.timecode.output.clone();
        self.mtc_out = None;
        let mut parts = Vec::new();
        if out.mtc {
            match MtcOutput::open(out.mtc_port.as_deref()) {
                Ok(m) => {
                    parts.push(format!("MTC to {}", m.port_name));
                    self.mtc_out = Some(m);
                }
                Err(e) => parts.push(format!("MTC out unavailable: {e}")),
            }
        }
        if out.ltc {
            parts.push(format!("LTC on output {}", out.ltc_channel));
        }
        self.ltc_sent = None;
        self.timecode_output = if parts.is_empty() { "off".into() } else { parts.join(", ") };
        if !parts.is_empty() {
            let level = if self.timecode_output.contains("unavailable") { LogLevel::Warn } else { LogLevel::Info };
            self.log(level, format!("timecode out: {}", self.timecode_output));
        }
    }

    /// Points the audio engine's LTC generator at the transport: at once on
    /// any change, otherwise every 250 ms (a restarted engine picks it up).
    fn update_ltc_out(&mut self, tc: &livestage_core::timecode::TcTransport, audio_clock: Option<ShowTime>) {
        use livestage_audio::ltc::LtcAnchor;
        let Some(audio) = self.audio.as_mut() else { return };
        let out = &self.bundle.project.timecode.output;
        let anchor = match audio_clock {
            Some(clock) if out.ltc => LtcAnchor {
                channel: Some(out.ltc_channel.max(1) as usize - 1),
                gain: livestage_audio::db_to_gain(out.ltc_level_db),
                rate: tc.rate,
                running: tc.running,
                at_frame: clock.to_frames(audio.sample_rate()),
                position_us: tc.position.as_micros() as f64,
                speed: if tc.speed.is_finite() { tc.speed } else { 1.0 },
            },
            _ => LtcAnchor::OFF,
        };
        let changed = |a: &LtcAnchor| (a.channel, a.running, a.rate, a.gain.to_bits()) != (anchor.channel, anchor.running, anchor.rate, anchor.gain.to_bits());
        let due = match &self.ltc_sent {
            None => true,
            Some((at, last, generation)) => {
                changed(last) || *generation != tc.generation || (anchor.running && at.elapsed() >= Duration::from_millis(250))
            }
        };
        if due {
            audio.set_ltc(anchor);
            self.ltc_sent = Some((Instant::now(), anchor, tc.generation));
        }
    }

    /// Opens the input for the configured timecode source (closing others).
    pub fn start_timecode_input(&mut self) {
        let cfg = self.bundle.project.timecode.clone();
        self.mtc_in = None;
        self.artnet_tc = None;
        self.ltc_in = None;
        // Stale samples from a previous source must not reach the new lock.
        while self.tc_rx.try_recv().is_ok() {}
        self.timecode_input = match cfg.source {
            TimecodeSource::Internal => "internal generator".into(),
            TimecodeSource::Mtc => match MtcInput::open(cfg.mtc_input.as_deref(), self.tc_tx.clone()) {
                Ok(m) => {
                    let s = format!("MTC on {}", m.port_name);
                    self.mtc_in = Some(m);
                    s
                }
                Err(e) => format!("MTC unavailable: {e}"),
            },
            TimecodeSource::ArtNet => match ArtNetTimecodeListener::start(cfg.artnet_port, self.tc_tx.clone()) {
                Ok(l) => {
                    let s = format!("Art-Net on UDP {}", l.port);
                    self.artnet_tc = Some(l);
                    s
                }
                Err(e) => format!("Art-Net port {} unavailable: {e}", cfg.artnet_port),
            },
            TimecodeSource::Ltc => match livestage_audio::ltc::LtcInput::open(cfg.ltc_input.as_deref(), cfg.ltc_channel, cfg.rate) {
                Ok(l) => {
                    let s = format!("LTC on {} channel {}", l.device_name, l.channel);
                    self.ltc_in = Some(l);
                    s
                }
                Err(e) => format!("LTC input unavailable: {e}"),
            },
        };
        let level = if self.timecode_input.contains("unavailable") { LogLevel::Warn } else { LogLevel::Info };
        self.log(level, format!("timecode: {}", self.timecode_input));
    }

    /// Rebuilds the DMX engine from the show's DMX configuration (patch,
    /// universes, transports), carrying the console state over.
    pub fn rebuild_dmx(&mut self) {
        let console = std::mem::take(&mut self.dmx.console);
        self.dmx = DmxEngine::new(self.bundle.project.dmx.clone());
        self.dmx.console = console;
        self.dmx_generation += 1;
        self.dmx.start_output();
        for e in self.dmx.patch_errors.clone() {
            self.log(LogLevel::Warn, format!("DMX patch: {e}"));
        }
    }

    /// Call after editing palettes: looks on stage that use them follow.
    pub fn palettes_edited(&mut self) {
        let now = self.now();
        self.dmx.palettes_changed(now, &self.bundle.project.lighting);
        self.mark_changed();
    }

    pub fn osc_port(&self) -> Option<u16> {
        self.osc.as_ref().map(|s| s.port)
    }
    pub fn ws_port(&self) -> Option<u16> {
        self.remote.as_ref().map(|s| s.port)
    }

    // ------------------------------------------------------------- lists --

    pub fn reset_lists(&mut self) {
        let now = self.now();
        let mut actions = std::mem::take(&mut self.actions);
        {
            let finished = self.video_finished.lock();
            let mut ctx = Ctx { now, actions: &mut actions, audio: self.audio.as_ref(), video_finished: &finished };
            for list in &mut self.bundle.project.cue_lists {
                list.reset(&mut ctx);
            }
        }
        self.actions = actions;
    }

    /// Runs `f` on a cue list (active one if `list` is None) with a cue context.
    fn with_list<R>(&mut self, list: Option<CueListId>, f: impl FnOnce(&mut CueList, &mut dyn CueContext) -> R) -> Option<R> {
        let now = self.now();
        let id = list.or(self.bundle.project.active_cue_list)?;
        let mut actions = std::mem::take(&mut self.actions);
        let result = {
            let finished = self.video_finished.lock();
            let mut ctx = Ctx { now, actions: &mut actions, audio: self.audio.as_ref(), video_finished: &finished };
            self.bundle.project.cue_lists.iter_mut().find(|l| l.id == id).map(|l| f(l, &mut ctx))
        };
        self.actions = actions;
        result
    }

    fn with_carts(&mut self, mut f: impl FnMut(&mut CueCart, &mut dyn CueContext)) {
        if self.bundle.project.carts.is_empty() {
            return;
        }
        let now = self.now();
        let mut actions = std::mem::take(&mut self.actions);
        {
            let finished = self.video_finished.lock();
            let mut ctx = Ctx { now, actions: &mut actions, audio: self.audio.as_ref(), video_finished: &finished };
            for k in &mut self.bundle.project.carts {
                f(k, &mut ctx);
            }
        }
        self.actions = actions;
    }

    fn with_all_lists(&mut self, mut f: impl FnMut(&mut CueList, &mut dyn CueContext)) {
        let now = self.now();
        let mut actions = std::mem::take(&mut self.actions);
        {
            let finished = self.video_finished.lock();
            let mut ctx = Ctx { now, actions: &mut actions, audio: self.audio.as_ref(), video_finished: &finished };
            for l in &mut self.bundle.project.cue_lists {
                f(l, &mut ctx);
            }
        }
        self.actions = actions;
    }

    pub fn command(&self, source: ControlSource, cmd: ShowCommand) {
        self.hub.send(source, cmd);
    }

    fn execute(&mut self, ev: ControlEvent) {
        let cmd = self.panic.filter(ev.command, Instant::now());
        if !matches!(cmd, ShowCommand::RequestStatus) {
            tracing::debug!(source = ?ev.source, ?cmd, "command");
        }
        match cmd {
            ShowCommand::Go { list } => {
                let fired = self.with_list(list, |l, c| l.go(c)).flatten();
                if fired.is_none() {
                    self.log(LogLevel::Warn, "GO: nothing on standby");
                }
            }
            ShowCommand::GoCue { number } => {
                if self.with_list(None, |l, c| l.go_number(c, &number)).flatten().is_none() {
                    self.log(LogLevel::Warn, format!("GO: no cue {number}"));
                }
            }
            ShowCommand::StandbyNext => {
                self.with_list(None, |l, c| l.standby_next(c));
            }
            ShowCommand::StandbyPrevious => {
                self.with_list(None, |l, c| l.standby_previous(c));
            }
            ShowCommand::SetStandby { number } => {
                self.with_list(None, |l, c| {
                    let i = l.index_of_number(&number);
                    if i.is_some() {
                        l.set_standby(c, i);
                    }
                });
            }
            ShowCommand::SeekCue { number, position } => {
                let id = self.bundle.project.active_list().and_then(|l| l.index_of_number(&number)).and_then(|i| {
                    self.bundle.project.active_list().map(|l| l.cues[i].header().id)
                });
                match id {
                    Some(id) => self.seek_cue(id, position),
                    None => self.log(LogLevel::Warn, format!("seek: no cue {number}")),
                }
            }
            ShowCommand::NextSlide => {
                self.slide_nav(None, SlideNav::Next);
            }
            ShowCommand::PrevSlide => {
                self.slide_nav(None, SlideNav::Previous);
            }
            ShowCommand::GotoSlide { slide } => {
                self.slide_nav(None, SlideNav::Goto(slide.saturating_sub(1)));
            }
            ShowCommand::PauseAll => {
                self.with_all_lists(|l, c| l.pause_all(c));
                self.with_carts(|k, c| k.pause_all(c));
            }
            ShowCommand::ResumeAll => {
                self.with_all_lists(|l, c| l.resume_all(c));
                self.with_carts(|k, c| k.resume_all(c));
            }
            ShowCommand::StopAll { fade } => {
                self.with_all_lists(|l, c| l.stop_all(c, fade));
                self.with_carts(|k, c| k.stop_all(c, fade));
            }
            ShowCommand::CartStopAll => self.with_carts(|k, c| k.stop_all(c, None)),
            ShowCommand::CartPad { cart, pad, down } => {
                let (ci, pi) = (cart.wrapping_sub(1), pad.wrapping_sub(1));
                let exists = self.bundle.project.carts.get(ci).is_some_and(|k| k.pad(pi).is_some());
                if !exists {
                    if down {
                        self.log(LogLevel::Warn, format!("cart {cart}: pad {pad} is empty"));
                    }
                } else {
                    let mut i = 0;
                    self.with_carts(|k, c| {
                        if i == ci {
                            if down {
                                k.press(c, pi);
                            } else {
                                k.release(c, pi);
                            }
                        }
                        i += 1;
                    });
                }
            }
            ShowCommand::Panic { hard } => {
                // Panic reaches every list, not just the active one.
                self.with_carts(|k, c| k.stop_all(c, Some(ShowTime::ZERO)));
                self.with_all_lists(|l, c| l.panic(c, hard));
                self.log(LogLevel::Warn, if hard { "HARD PANIC" } else { "PANIC (fade)" });
            }
            ShowCommand::SetMasterGain { db } => {
                self.bundle.project.audio.master_gain_db = db;
                self.actions.push(EngineAction::Audio(AudioAction::SetMasterGain { db }));
            }
            ShowCommand::SetOutputsFullscreen { enabled } => self.outputs_live_request = Some(enabled),
            ShowCommand::SetSubmaster { sub, level } => {
                match self.bundle.project.lighting.submasters.get_mut(sub.wrapping_sub(1)) {
                    Some(s) => s.level = level.clamp(0.0, 1.0),
                    None => self.log(LogLevel::Warn, format!("no submaster {sub}")),
                }
            }
            ShowCommand::FlashSubmaster { sub, down } => {
                if let Some(id) = self.bundle.project.lighting.submasters.get(sub.wrapping_sub(1)).map(|s| s.id) {
                    if down {
                        self.dmx.console.flashes.insert(id);
                    } else {
                        self.dmx.console.flashes.remove(&id);
                    }
                }
            }
            ShowCommand::SetGrandMaster { level } => self.bundle.project.lighting.grand_master = level.clamp(0.0, 1.0),
            ShowCommand::SetBlackout { enabled } => self.dmx.console.blackout = enabled,
            ShowCommand::SetEffect { effect, running } => {
                let now = self.now();
                match self.bundle.project.lighting.effects.get(effect.wrapping_sub(1)).map(|e| e.id) {
                    Some(id) => self.dmx.console.set_effect(id, running, now),
                    None => self.log(LogLevel::Warn, format!("no effect {effect}")),
                }
            }
            ShowCommand::ClearProgrammer => self.dmx.console.programmer.clear(),
            ShowCommand::TimecodeStart | ShowCommand::TimecodeStop | ShowCommand::TimecodeLocate { .. } => {
                let applied = match cmd {
                    ShowCommand::TimecodeStart => self.timecode.start(),
                    ShowCommand::TimecodeStop => self.timecode.stop(),
                    ShowCommand::TimecodeLocate { timecode } => self.timecode.locate(timecode),
                    _ => unreachable!(),
                };
                if !applied {
                    self.log(LogLevel::Warn, "timecode: the external source controls the transport");
                }
            }
            ShowCommand::RequestStatus => self.last_status = Instant::now() - std::time::Duration::from_secs(1),
        }
    }

    fn flush_actions(&mut self) {
        let now = self.now();
        let actions = std::mem::take(&mut self.actions);
        let bundle = &self.bundle;
        let resolve = |a: &AssetRef| bundle.resolve(a).ok();
        let mut follow_up = Vec::new();
        for action in &actions {
            match action {
                EngineAction::Audio(a) => {
                    if let Some(engine) = self.audio.as_mut() {
                        engine.handle(a, &resolve);
                    }
                }
                EngineAction::Video(v) => {
                    let _ = self.video_tx.send((now, v.clone()));
                }
                EngineAction::Dmx(d) => self.dmx.handle(now, d, &bundle.project.lighting),
                EngineAction::Control(c) => follow_up.push(c.clone()),
            }
        }
        for c in follow_up {
            match c {
                ControlAction::SendOsc { host, port, address, args } => {
                    if let Some(s) = &self.osc_out {
                        if let Err(e) = s.send(&host, port, &address, &args) {
                            self.log(LogLevel::Error, format!("OSC send {address}: {e}"));
                        }
                    }
                }
                ControlAction::SendMidi { port, bytes } => {
                    if self.midi_out.is_none() {
                        self.midi_out = MidiOutputHandler::open(port.as_deref()).ok();
                    }
                    match &mut self.midi_out {
                        Some(m) => {
                            if let Err(e) = m.send(&bytes) {
                                self.log(LogLevel::Error, format!("MIDI send: {e}"));
                            }
                        }
                        None => self.log(LogLevel::Warn, "MIDI cue skipped: no output port"),
                    }
                }
                ControlAction::Command(cmd) => self.hub.send(ControlSource::Cue, cmd),
                ControlAction::Marker { text } => self.log(LogLevel::Info, format!("MARKER: {text}")),
            }
        }
    }

    /// Scrubs any cue (any list, timeline children included) to `position`,
    /// in place if it is playing.
    pub fn seek_cue(&mut self, id: CueId, position: ShowTime) {
        let mut done = false;
        self.with_all_lists(|l, c| {
            if !done {
                done = l.seek_cue(c, id, position);
            }
        });
        if !done {
            self.with_carts(|k, c| {
                if let Some(p) = k.pads.iter_mut().flatten().find(|p| p.cue.header().id == id) {
                    p.cue.seek(c, position);
                }
            });
        }
    }

    /// Pauses / resumes / stops one cue (timeline children included).
    pub fn cue_op(&mut self, id: CueId, op: CueOp) {
        let now = self.now();
        let mut actions = std::mem::take(&mut self.actions);
        {
            let finished = self.video_finished.lock();
            let mut ctx = Ctx { now, actions: &mut actions, audio: self.audio.as_ref(), video_finished: &finished };
            if let Some(c) = find_cue_in(&mut self.bundle.project.cue_lists, &mut self.bundle.project.carts, id) {
                match op {
                    CueOp::Pause => c.pause(&mut ctx),
                    CueOp::Resume => c.resume(&mut ctx),
                    CueOp::Stop => c.stop(&mut ctx, None),
                }
            }
        }
        self.actions = actions;
    }

    /// Next / previous / go-to on a presentation (`None` = the current one).
    pub fn slide_nav(&mut self, target: Option<CueId>, nav: SlideNav) -> bool {
        let Some(id) = target.or_else(|| current_slides(&self.bundle.project)) else {
            self.log(LogLevel::Warn, "slides: no presentation is running");
            return false;
        };
        let now = self.now();
        let mut actions = std::mem::take(&mut self.actions);
        let changed = {
            let finished = self.video_finished.lock();
            let mut ctx = Ctx { now, actions: &mut actions, audio: self.audio.as_ref(), video_finished: &finished };
            match find_cue_in(&mut self.bundle.project.cue_lists, &mut self.bundle.project.carts, id) {
                Some(AnyCue::Slides(s)) => match nav {
                    SlideNav::Next => s.next(&mut ctx),
                    SlideNav::Previous => s.previous(&mut ctx),
                    SlideNav::Goto(i) => s.goto(&mut ctx, i),
                },
                _ => false,
            }
        };
        self.actions = actions;
        changed
    }

    /// Imports a PDF / PowerPoint: the original is copied into the bundle,
    /// then rendered to slides on a background thread (never under the
    /// runtime lock). The slide cue is added when rendering finishes.
    fn start_document_import(&mut self, rec: &livestage_core::project::AssetRecord, name: String) {
        let src = match self.bundle.resolve(&rec.path) {
            Ok(p) => p,
            Err(e) => return self.log(LogLevel::Error, format!("import: {e}")),
        };
        let (dir, prefix) = match self.bundle.new_slides_dir(&name) {
            Ok(d) => d,
            Err(e) => return self.log(LogLevel::Error, format!("import: {e}")),
        };
        // Render at the resolution of the largest output (4K at most).
        let target = self
            .bundle
            .project
            .displays
            .iter()
            .filter(|d| d.enabled)
            .filter_map(|d| d.canvas_size)
            .max_by_key(|[w, h]| w * h)
            .map_or((1920, 1080), |[w, h]| (w.min(3840), h.min(2160)));
        let office = self.office_path.clone().filter(|p| p.is_file()).or_else(livestage_video::slides::find_office);
        let opts = livestage_video::slides::ImportOptions { target, office };
        let progress = Arc::new((std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicUsize::new(0)));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let (p, source, out) = (progress.clone(), rec.path.clone(), dir.clone());
        let spawned = std::thread::Builder::new().name("livestage-import".into()).spawn(move || {
            let result = livestage_video::slides::import_document(&src, Some(source), &out, &prefix, &opts, &mut |done, total| {
                p.0.store(done, Ordering::Relaxed);
                p.1.store(total, Ordering::Relaxed);
            });
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
        match spawned {
            Ok(_) => {
                self.log(LogLevel::Info, format!("rendering {} to slides...", rec.path));
                self.imports.push(DocImport { name, progress, result: rx, dir });
            }
            Err(e) => self.log(LogLevel::Error, format!("import thread: {e}")),
        }
    }

    /// Adds slide cues for finished document imports.
    fn poll_imports(&mut self) {
        let mut finished = Vec::new();
        self.imports.retain(|job| match job.result.try_recv() {
            Ok(r) => {
                finished.push((job.name.clone(), job.dir.clone(), r));
                false
            }
            Err(crossbeam_channel::TryRecvError::Empty) => true,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                finished.push((job.name.clone(), job.dir.clone(), Err("import thread stopped".into())));
                false
            }
        });
        for (name, dir, result) in finished {
            match result {
                Ok(deck) => {
                    let number = next_cue_number(&self.bundle.project);
                    let n = deck.slides.len();
                    let cue = crate::demo::slide_cue(&self.bundle.project, &number, &name, deck);
                    if let Some(l) = self.bundle.project.active_list_mut() {
                        l.cues.push(cue);
                    }
                    self.after_structure_edit();
                    self.log(LogLevel::Info, format!("imported \"{name}\" as cue {number} ({n} slides)"));
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dir);
                    self.log(LogLevel::Error, format!("import of \"{name}\" failed: {e}"));
                }
            }
        }
    }

    /// One show-control frame. Call at display rate or faster.
    pub fn tick(&mut self) {
        if !self.imports.is_empty() {
            self.poll_imports();
        }
        let events: Vec<ControlEvent> = self.hub.drain().collect();
        for ev in events {
            self.execute(ev);
        }
        // Hand the engines what commands (and UI work queued since the last
        // tick) produced before the lists tick: a seek must reach the audio
        // engine before a timeline reads its master's clock, or the timeline
        // would chase the stale position.
        self.flush_actions();
        // Timecode: feed received samples (stamped at arrival), advance the
        // lock / generator, then let slaved timelines and lists chase it.
        for input in self.tc_rx.try_iter() {
            let local: ShowTime = input.at.saturating_duration_since(self.epoch).into();
            self.timecode.feed(local, input.origin, input.sample);
        }
        if let Some(ltc) = &mut self.ltc_in {
            for r in ltc.drain() {
                let local: ShowTime = r.at.saturating_duration_since(self.epoch).into();
                let sample = livestage_core::timecode::ltc::frame_end_sample(&r.timecode);
                self.timecode.feed(local, TimecodeSource::Ltc, sample);
            }
        }
        let audio_clock = self.audio.as_ref().and_then(|a| a.sample_clock());
        let tc = self.timecode.update(self.now(), audio_clock);
        // Timecode out: MTC from this thread, LTC from the audio engine.
        if let Some(m) = &mut self.mtc_out {
            m.update(&tc);
        }
        self.update_ltc_out(&tc, audio_clock);
        self.with_all_lists(|l, c| l.follow_timecode(c, &tc));
        self.with_all_lists(|l, c| l.tick(c));
        self.with_carts(|k, c| k.tick(c));
        self.flush_actions();

        let now = self.now();
        if let Some(a) = &mut self.audio {
            a.maintenance();
        }
        self.dmx.tick(now, &self.bundle.project.lighting);
        self.publish_status();
    }

    fn publish_status(&mut self) {
        if self.last_status.elapsed().as_millis() < 100 {
            return;
        }
        self.last_status = Instant::now();
        let Some(remote) = &self.remote else { return };
        let status = self.status();
        let doc = serde_json::json!({
            "type": "status",
            "project": self.bundle.project.name,
            "outputs_live": self.outputs_live,
            "timecode": {
                "timecode": self.timecode.transport().timecode().to_string(),
                "rate": self.timecode.transport().rate,
                "running": self.timecode.transport().running,
                "state": self.timecode.transport().state,
                "source": self.timecode.config().source,
            },
            "list": status,
        });
        remote.publish_status(doc.to_string());
    }

    pub fn status(&self) -> Option<CueListStatus> {
        let mut scratch = Vec::new();
        let finished = self.video_finished.lock();
        let ctx = Ctx { now: self.now(), actions: &mut scratch, audio: self.audio.as_ref(), video_finished: &finished };
        self.bundle.project.active_list().map(|l| l.status(&ctx))
    }

    // --------------------------------------------------------- authoring --

    pub fn save(&mut self) {
        match self.bundle.save() {
            Ok(()) => {
                self.dirty = false;
                let p = self.bundle.manifest_path().display().to_string();
                self.log(LogLevel::Info, format!("saved {p}"));
            }
            Err(e) => self.log(LogLevel::Error, format!("save failed: {e}")),
        }
    }

    /// Imports a file into `assets/` and appends a matching cue.
    pub fn import_and_create_cue(&mut self, path: &Path) {
        // Accept paths pasted from Finder / shells (quoted, or with escaped spaces).
        let cleaned = path.to_string_lossy().trim().trim_matches(|c| c == '"' || c == '\'').replace("\\ ", " ");
        let path = Path::new(&cleaned);
        if cleaned.is_empty() {
            return self.log(LogLevel::Warn, "import: enter a file path first");
        }
        if !path.is_file() {
            return self.log(LogLevel::Error, format!("import: file not found: {}", path.display()));
        }
        let rec = match self.bundle.import_asset(path) {
            Ok(r) => r,
            Err(e) => return self.log(LogLevel::Error, format!("import failed: {e}")),
        };
        let number = next_cue_number(&self.bundle.project);
        let name = rec.original_name.rsplit_once('.').map_or(rec.original_name.clone(), |(n, _)| n.to_string());
        let cue = match rec.kind {
            AssetKind::Audio | AssetKind::Video | AssetKind::Image => match cue_for_asset(self.bundle.root(), &self.bundle.project, &rec, &number, &name) {
                Some(c) => c,
                None => return,
            },
            AssetKind::Document => {
                self.mark_changed();
                return self.start_document_import(&rec, name);
            }
            AssetKind::Data => {
                self.log(LogLevel::Info, format!("imported data file {}", rec.path));
                self.mark_changed();
                return;
            }
        };
        if let Some(l) = self.bundle.project.active_list_mut() {
            l.cues.push(cue);
        }
        self.mark_changed();
        self.log(LogLevel::Info, format!("imported {} as cue {number}", rec.path));
    }

    /// Records an edit (dirty flag, undo, autosave).
    pub fn mark_changed(&mut self) {
        self.dirty = true;
        self.undo.touch();
        self.edit_generation += 1;
        self.refresh_midi_bindings();
    }

    /// Operator MIDI bindings: cart pads first (note-on presses, note-off
    /// releases), then the default GO / STOP / PANIC notes.
    pub fn midi_bindings(&self) -> Vec<livestage_control::midi::MidiBinding> {
        use livestage_control::midi::{MidiBinding, MidiTrigger};
        let mut b = Vec::new();
        for (ci, cart) in self.bundle.project.carts.iter().enumerate() {
            for (pi, pad) in cart.pads.iter().enumerate() {
                let Some(n) = pad.as_ref().and_then(|p| p.midi) else { continue };
                let cmd = |down| ShowCommand::CartPad { cart: ci + 1, pad: pi + 1, down };
                b.push(MidiBinding { trigger: MidiTrigger::Note { channel: n.channel, note: n.note }, command: cmd(true) });
                b.push(MidiBinding { trigger: MidiTrigger::NoteOff { channel: n.channel, note: n.note }, command: cmd(false) });
            }
        }
        b.extend(livestage_control::midi::default_bindings());
        b
    }

    /// Pushes the current bindings to the open MIDI input (no reconnect).
    pub fn refresh_midi_bindings(&self) {
        if let Some(m) = &self.midi_in {
            m.set_bindings(self.midi_bindings());
        }
    }

    pub fn undo(&mut self) {
        let current = &self.bundle.project;
        if let Some(p) = self.undo.undo(current) {
            self.restore(p, "undo");
        }
    }

    pub fn redo(&mut self) {
        let current = &self.bundle.project;
        if let Some(p) = self.undo.redo(current) {
            self.restore(p, "redo");
        }
    }

    /// Swaps in a restored project while keeping the show running: live cue
    /// state is adopted from the current project, and live levels survive.
    pub fn restore(&mut self, mut project: livestage_core::Project, what: &str) {
        let old = std::mem::replace(&mut self.bundle.project, livestage_core::Project::new(""));
        if project.displays != old.displays {
            self.touch_displays();
        }
        project.adopt_runtime_from(&old);
        project.audio.master_gain_db = old.audio.master_gain_db;
        let dmx_changed = project.dmx != old.dmx;
        // Fader positions are live controls: undo never moves them.
        project.lighting.set_levels(&old.lighting.levels());
        let palettes_changed = project.lighting.palettes != old.lighting.palettes;
        self.bundle.project = project;
        if palettes_changed {
            let now = self.now();
            self.dmx.palettes_changed(now, &self.bundle.project.lighting);
        }
        self.apply_audio_routing();
        if self.bundle.project.timecode != *self.timecode.config() {
            self.apply_timecode_config();
        }
        self.refresh_midi_bindings();
        if dmx_changed {
            self.rebuild_dmx();
        }
        // If the standby cue no longer exists, put the playhead on the first cue.
        self.with_all_lists(|l, c| {
            if l.standby_index().is_none() && !l.cues.is_empty() && l.last_go_index().is_none() {
                l.set_standby(c, Some(0));
            }
        });
        self.dirty = true;
        self.edit_generation += 1;
        self.log(LogLevel::Info, what.to_string());
    }

    /// Replaces the project with the autosaved copy.
    pub fn recover_autosave(&mut self) {
        match self.bundle.load_autosave() {
            Ok(p) => {
                self.restore(p, "recovered unsaved changes from autosave");
                let p = self.bundle.project.clone();
                self.undo.reset(&p);
                self.reset_lists();
            }
            Err(e) => self.log(LogLevel::Error, format!("recovery failed: {e}")),
        }
    }

    /// Re-standbys the list after structural edits (insert/delete/reorder).
    pub fn after_structure_edit(&mut self) {
        self.mark_changed();
        self.with_list(None, |l, c| {
            let keep = l.standby_index().filter(|&i| i < l.cues.len());
            l.set_standby(c, keep.or(if l.cues.is_empty() { None } else { Some(0) }));
        });
    }


}

// ---------------------------------------------------------------- queries --
// Shared by the runtime and the UI's frame copy (crate::view).

/// The most recently started running presentation, in any list.
pub fn current_slides(project: &livestage_core::Project) -> Option<CueId> {
    fn walk<'a>(c: &'a AnyCue, out: &mut Vec<&'a livestage_core::cue::SlideCue>) {
        match c {
            AnyCue::Slides(s) if s.state().is_active() => out.push(s),
            AnyCue::Timeline(t) => t.items.iter().for_each(|i| walk(&i.cue, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    for c in project.cue_lists.iter().flat_map(|l| &l.cues) {
        walk(c, &mut found);
    }
    found.into_iter().max_by_key(|s| s.header.runtime.started_at).map(|s| s.header.id)
}

/// The next whole cue number after the highest numeric one in the active list.
pub fn next_cue_number(project: &livestage_core::Project) -> String {
    let max = project
        .active_list()
        .map(|l| l.cues.iter().filter_map(|c| c.header().number.parse::<f64>().ok()).fold(0.0, f64::max))
        .unwrap_or(0.0);
    format!("{}", max.floor() as i64 + 1)
}

/// A playable cue for an imported audio / video / image asset.
pub fn cue_for_asset(root: &Path, project: &livestage_core::Project, rec: &livestage_core::project::AssetRecord, number: &str, name: &str) -> Option<AnyCue> {
    match rec.kind {
        AssetKind::Audio => {
            let media_length = livestage_core::bundle::resolve_asset(root, &rec.path)
                .ok()
                .and_then(|p| livestage_audio::decode::probe(&p).ok())
                .and_then(|i| i.duration_secs())
                .map(ShowTime::from_secs_f64);
            Some(crate::demo::audio_cue(project, number, name, rec.path.clone(), media_length))
        }
        AssetKind::Video | AssetKind::Image => Some(crate::demo::visual_cue(project, number, name, rec.path.clone(), rec.kind == AssetKind::Video)),
        AssetKind::Document | AssetKind::Data => None,
    }
}

/// Flattened rows (timeline children indented) for the cue grid.
pub fn cue_rows(project: &livestage_core::Project, ctx: &dyn CueContext) -> Vec<CueRow> {
    let Some(list) = project.active_list() else { return vec![] };
    let standby = list.standby_cue().map(|c| c.header().id);
    let mut rows = Vec::new();
    fn push(rows: &mut Vec<CueRow>, c: &AnyCue, depth: usize, offset: Option<ShowTime>, ctx: &dyn CueContext, standby: Option<CueId>) {
        let h = c.header();
        rows.push(CueRow {
            id: h.id,
            depth,
            number: h.number.clone(),
            name: h.name.clone(),
            kind: c.kind().label(),
            state: c.state(),
            pre_wait: h.pre_wait,
            post_wait: h.post_wait,
            duration: c.duration(),
            elapsed: c.elapsed(ctx),
            progress: c.progress(ctx),
            follow: h.follow,
            armed: h.armed,
            offset,
            is_standby: standby == Some(h.id),
            detail: match c {
                AnyCue::Slides(s) if s.slide_count() > 0 => {
                    let at = if s.state().is_active() { s.current } else { s.start_slide };
                    Some(format!("Slide {} / {}", at + 1, s.slide_count()))
                }
                _ => None,
            },
            timecode: match c {
                AnyCue::Timeline(t) => t.timecode.as_ref().map(|s| s.start).or(h.timecode),
                _ => h.timecode,
            },
        });
        if let AnyCue::Timeline(t) = c {
            if let Some(m) = &t.master {
                push(rows, m, depth + 1, Some(ShowTime::ZERO), ctx, None);
            }
            for i in &t.items {
                push(rows, &i.cue, depth + 1, Some(i.offset), ctx, None);
            }
        }
    }
    for c in &list.cues {
        push(&mut rows, c, 0, None, ctx, standby);
    }
    rows
}
