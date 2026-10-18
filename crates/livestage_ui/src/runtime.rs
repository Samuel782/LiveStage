//! Show runtime: owns the project bundle and every engine, executes
//! [`ShowCommand`]s and routes cue [`EngineAction`]s to engines.

use livestage_audio::{AudioEngine, EngineConfig};
use livestage_control::midi::{MidiInputHandler, MidiOutputHandler};
use livestage_control::osc::{OscSender, OscServer};
use livestage_control::websocket::RemoteServer;
use livestage_control::{ControlEvent, ControlHub, ControlSource, PanicController};
use livestage_core::action::{AudioAction, ControlAction, EngineAction};
use livestage_core::cue::{AnyCue, Cue, CueContext, CueExt};
use livestage_core::cuelist::{CueList, CueListStatus};
use livestage_core::id::{CueId, CueListId};
use livestage_core::project::{AssetKind, AssetRef};
use livestage_core::{ProjectBundle, ShowCommand, ShowTime};
use livestage_dmx::DmxEngine;
use livestage_core::action::VideoAction;
use parking_lot::Mutex;
use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Video actions stamped with the show time they were dispatched at, sent to
/// the render thread which owns the compositor.
pub type VideoMsg = (ShowTime, VideoAction);

/// Cues whose video media has ended, published by the render thread.
pub type VideoFinished = Arc<Mutex<HashSet<CueId>>>;

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
}

impl ShowRuntime {
    pub fn new(bundle: ProjectBundle) -> Self {
        let (video_tx, video_rx) = crossbeam_channel::unbounded();
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
            bundle,
        };
        for e in rt.dmx.patch_errors.clone() {
            rt.log(LogLevel::Warn, format!("DMX patch: {e}"));
        }
        rt.start_audio();
        rt.start_control();
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

    /// The show clock origin (shared with the render thread).
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Receiver for video actions; drained by the render thread.
    pub fn video_receiver(&self) -> crossbeam_channel::Receiver<VideoMsg> {
        self.video_rx.clone()
    }

    /// Sends a video action directly (operator tools such as test patterns).
    pub fn send_video(&self, action: VideoAction) {
        let _ = self.video_tx.send((self.now(), action));
    }

    pub fn log(&mut self, level: LogLevel, text: impl Into<String>) {
        let text = text.into();
        match level {
            LogLevel::Info => tracing::info!("{text}"),
            LogLevel::Warn => tracing::warn!("{text}"),
            LogLevel::Error => tracing::error!("{text}"),
        }
        self.log.push_back(LogLine { at: self.now(), text, level });
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
        match MidiInputHandler::open(port, livestage_control::midi::default_bindings(), id, self.hub.sender()) {
            Ok(h) => {
                let msg = format!("MIDI input: {}", h.port_name);
                self.midi_in = Some(h);
                self.log(LogLevel::Info, msg);
            }
            Err(e) => tracing::info!("MIDI input not opened: {e}"),
        }
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
            ShowCommand::PauseAll => self.with_all_lists(|l, c| l.pause_all(c)),
            ShowCommand::ResumeAll => self.with_all_lists(|l, c| l.resume_all(c)),
            ShowCommand::StopAll { fade } => self.with_all_lists(|l, c| l.stop_all(c, fade)),
            ShowCommand::Panic { hard } => {
                // Panic reaches every list, not just the active one.
                self.with_all_lists(|l, c| l.panic(c, hard));
                self.log(LogLevel::Warn, if hard { "HARD PANIC" } else { "PANIC (fade)" });
            }
            ShowCommand::SetMasterGain { db } => {
                self.bundle.project.audio.master_gain_db = db;
                self.actions.push(EngineAction::Audio(AudioAction::SetMasterGain { db }));
            }
            ShowCommand::SetOutputsFullscreen { enabled } => self.outputs_live_request = Some(enabled),
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
                EngineAction::Dmx(d) => self.dmx.handle(now, d),
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

    /// One show-control frame. Call at display rate or faster.
    pub fn tick(&mut self) {
        let events: Vec<ControlEvent> = self.hub.drain().collect();
        for ev in events {
            self.execute(ev);
        }
        self.with_all_lists(|l, c| l.tick(c));
        self.flush_actions();

        let now = self.now();
        if let Some(a) = &mut self.audio {
            a.maintenance();
        }
        self.dmx.tick(now);
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

    /// Flattened rows (timeline children indented) for the cue grid.
    pub fn cue_rows(&self) -> Vec<CueRow> {
        let mut scratch = Vec::new();
        let finished = self.video_finished.lock();
        let ctx = Ctx { now: self.now(), actions: &mut scratch, audio: self.audio.as_ref(), video_finished: &finished };
        let Some(list) = self.bundle.project.active_list() else { return vec![] };
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
            push(&mut rows, c, 0, None, &ctx, standby);
        }
        rows
    }


    /// Elapsed time of any cue (searching all lists and timelines).
    pub fn elapsed_of(&self, id: CueId) -> Option<ShowTime> {
        let mut scratch = Vec::new();
        let finished = self.video_finished.lock();
        let ctx = Ctx { now: self.now(), actions: &mut scratch, audio: self.audio.as_ref(), video_finished: &finished };
        let cue = self.bundle.project.cue_lists.iter().find_map(|l| l.find(id))?;
        cue.state().is_active().then(|| cue.elapsed(&ctx))
    }

    pub fn add_cue_list(&mut self, name: &str) -> CueListId {
        let mut list = CueList::new(name);
        let id = list.id;
        {
            let now = self.now();
            let finished = self.video_finished.lock();
            let mut scratch = Vec::new();
            let mut ctx = Ctx { now, actions: &mut scratch, audio: self.audio.as_ref(), video_finished: &finished };
            list.reset(&mut ctx);
        }
        self.bundle.project.cue_lists.push(list);
        self.bundle.project.active_cue_list = Some(id);
        self.mark_changed();
        id
    }

    /// The most recently started running cue: (number, name, elapsed, remaining).
    /// Drives the large countdown in the transport bar.
    pub fn running_summary(&self) -> Option<(String, String, ShowTime, Option<ShowTime>)> {
        let list = self.bundle.project.active_list()?;
        let cue = list
            .cues
            .iter()
            .filter(|c| c.state().is_active())
            .max_by_key(|c| c.header().runtime.started_at)?;
        let mut scratch = Vec::new();
        let finished = self.video_finished.lock();
        let ctx = Ctx { now: self.now(), actions: &mut scratch, audio: self.audio.as_ref(), video_finished: &finished };
        let h = cue.header();
        Some((h.number.clone(), h.name.clone(), cue.elapsed(&ctx), cue.remaining(&ctx)))
    }

    pub fn running_count(&self) -> usize {
        self.bundle.project.active_list().map_or(0, |l| l.cues.iter().filter(|c| c.state().is_active()).count())
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
        let number = self.next_cue_number();
        let name = rec.original_name.rsplit_once('.').map_or(rec.original_name.clone(), |(n, _)| n.to_string());
        let cue = match rec.kind {
            AssetKind::Audio => {
                let media_length = self
                    .bundle
                    .resolve(&rec.path)
                    .ok()
                    .and_then(|p| livestage_audio::decode::probe(&p).ok())
                    .and_then(|i| i.duration_secs())
                    .map(ShowTime::from_secs_f64);
                crate::demo::audio_cue(&self.bundle.project, &number, &name, rec.path.clone(), media_length)
            }
            AssetKind::Video | AssetKind::Image => {
                crate::demo::visual_cue(&self.bundle.project, &number, &name, rec.path.clone(), rec.kind == AssetKind::Video)
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

    pub fn next_cue_number(&self) -> String {
        let max = self
            .bundle
            .project
            .active_list()
            .map(|l| l.cues.iter().filter_map(|c| c.header().number.parse::<f64>().ok()).fold(0.0, f64::max))
            .unwrap_or(0.0);
        format!("{}", max.floor() as i64 + 1)
    }

    /// Records an edit (dirty flag, undo, autosave).
    pub fn mark_changed(&mut self) {
        self.dirty = true;
        self.undo.touch();
        self.edit_generation += 1;
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
        project.adopt_runtime_from(&old);
        project.audio.master_gain_db = old.audio.master_gain_db;
        let dmx_changed = project.dmx != old.dmx;
        self.bundle.project = project;
        self.apply_audio_routing();
        if dmx_changed {
            self.dmx = DmxEngine::new(self.bundle.project.dmx.clone());
            self.dmx.start_output();
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

    pub fn find_cue_mut(&mut self, id: CueId) -> Option<&mut AnyCue> {
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
        self.bundle.project.cue_lists.iter_mut().flat_map(|l| l.cues.iter_mut()).find_map(|c| find(c, id))
    }
}
