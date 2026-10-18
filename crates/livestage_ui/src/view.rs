//! The UI's per-frame copy of the show runtime.
//!
//! The control window used to lay out every panel while holding the runtime
//! lock, so a slow frame (a long cue list, the lighting plot, a font atlas
//! rebuild) delayed the 1 kHz show thread by the whole layout time. Now a
//! frame is three steps:
//!
//! 1. [`UiRuntime::sync`] copies what the panels read (project, engine
//!    status, DMX output, log) while holding the lock. Copy only, no layout.
//! 2. Panels lay out against the copy **without the lock**. Edits change the
//!    copied project; engine work (seek, save, undo, restart audio, …) is
//!    queued as operations.
//! 3. [`UiRuntime::take_edits`] works out what the panels changed (still
//!    without the lock), then [`UiRuntime::apply`] takes the lock again,
//!    moves the edits into the live project ([`livestage_core::edits`]: live
//!    cue state, OSC fader moves and finished imports survive) and runs the
//!    queued operations.
//!
//! The show thread therefore only ever waits for steps 1 and 3, which are
//! bounded by the size of the show, not by how long egui takes.

use crate::runtime::{renew_ids, CueRow, CueOp, LogLevel, LogLine, ShowRuntime, ShowStats, SlideNav};
use livestage_control::{ControlHub, ControlSource};
use livestage_core::action::{EngineAction, VideoAction};
use livestage_core::cart::CueCart;
use livestage_core::cue::{AnyCue, Cue, CueContext, CueExt};
use livestage_core::cuelist::CueList;
use livestage_core::edits::ProjectEdits;
use livestage_core::id::{CueId, CueListId, EffectId, FixtureId, SubmasterId};
use livestage_core::lighting::{LightValue, Palette, PaletteKind};
use livestage_core::project::{AssetRecord, AssetRef};
use livestage_core::timecode::{TcTransport, TimecodeConfig};
use livestage_core::{Project, ShowCommand, ShowTime};
use livestage_dmx::console::{Programmer, Slot};
use livestage_dmx::fixture::{FixtureLibrary, Patch};
use livestage_dmx::Universe;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Engine work queued by the UI, run under the lock in [`UiRuntime::apply`].
type Op = Box<dyn FnOnce(&mut ShowRuntime) + Send>;

/// Cue context for read-only queries on the copy.
struct ViewCtx<'a> {
    now: ShowTime,
    media: &'a HashMap<CueId, (Option<ShowTime>, bool)>,
    video_finished: &'a HashSet<CueId>,
    scratch: Vec<EngineAction>,
}

impl CueContext for ViewCtx<'_> {
    fn now(&self) -> ShowTime {
        self.now
    }
    fn dispatch(&mut self, action: EngineAction) {
        self.scratch.push(action);
    }
    fn media_position(&self, cue: CueId) -> Option<ShowTime> {
        self.media.get(&cue).and_then(|m| m.0)
    }
    fn media_finished(&self, cue: CueId) -> bool {
        self.media.get(&cue).is_some_and(|m| m.1) || self.video_finished.contains(&cue)
    }
}

/// The project bundle as the panels see it.
pub struct BundleView {
    root: PathBuf,
    pub project: Project,
}

impl BundleView {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn resolve(&self, asset: &AssetRef) -> livestage_core::CoreResult<PathBuf> {
        livestage_core::bundle::resolve_asset(&self.root, asset)
    }
}

/// Audio engine status.
pub struct AudioView {
    device_name: String,
    sample_rate: u32,
    channels: u16,
    dsp_load: f32,
    leaked: u64,
    peaks: Vec<f32>,
}

impl AudioView {
    pub fn device_name(&self) -> &str {
        &self.device_name
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn dsp_load(&self) -> f32 {
        self.dsp_load
    }
    /// Real-time command queue overflows since the engine started.
    pub fn leaked(&self) -> u64 {
        self.leaked
    }
    /// Peak per output since the previous frame.
    pub fn take_peaks(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.peaks)
    }
}

/// Timecode engine status; `reset` is applied after the frame.
#[derive(Default)]
pub struct TimecodeView {
    config: TimecodeConfig,
    transport: TcTransport,
    lock_error_us: f64,
    reset_requested: bool,
}

impl TimecodeView {
    pub fn config(&self) -> &TimecodeConfig {
        &self.config
    }
    pub fn transport(&self) -> TcTransport {
        self.transport
    }
    pub fn lock_error_us(&self) -> f64 {
        self.lock_error_us
    }
    pub fn reset(&mut self) {
        self.reset_requested = true;
    }
}

/// The open MIDI input: name and the MIDI-learn slot (shared with the port thread).
pub struct MidiView {
    pub port_name: String,
    learned: Arc<parking_lot::Mutex<Option<(u8, u8)>>>,
}

impl MidiView {
    pub fn take_learned(&self) -> Option<(u8, u8)> {
        self.learned.lock().take()
    }
}

/// A presentation render in progress.
pub struct ImportView {
    pub name: String,
    pub progress: Arc<(std::sync::atomic::AtomicUsize, std::sync::atomic::AtomicUsize)>,
}

#[derive(Default)]
pub struct UndoView {
    can_undo: bool,
    can_redo: bool,
}

impl UndoView {
    pub fn can_undo(&self) -> bool {
        self.can_undo
    }
    pub fn can_redo(&self) -> bool {
        self.can_redo
    }
}

/// Lighting console state the operator edits directly. Programmer,
/// flashes and blackout are merged back field by field; effect toggles are
/// queued.
#[derive(Default)]
pub struct ConsoleView {
    pub programmer: Programmer,
    pub flashes: HashSet<SubmasterId>,
    pub blackout: bool,
    base: (Programmer, HashSet<SubmasterId>, bool),
    running: HashSet<EffectId>,
    console_fx: Vec<EffectId>,
    fx_changes: Vec<(EffectId, bool, ShowTime)>,
}

impl ConsoleView {
    pub fn set_effect(&mut self, id: EffectId, running: bool, now: ShowTime) {
        if running {
            self.running.insert(id);
            if !self.console_fx.contains(&id) {
                self.console_fx.push(id);
            }
        } else {
            self.running.remove(&id);
            self.console_fx.retain(|e| *e != id);
        }
        self.fx_changes.push((id, running, now));
    }
    pub fn effect_running(&self, id: EffectId) -> bool {
        self.running.contains(&id)
    }
    /// Effects started from the console (recorded into cues).
    pub fn console_effects(&self) -> Vec<EffectId> {
        self.console_fx.clone()
    }
}

/// DMX engine state: console, patch and the last output frame.
pub struct DmxView {
    pub console: ConsoleView,
    pub library: Arc<FixtureLibrary>,
    pub patch_errors: Vec<String>,
    patch: Arc<Patch>,
    last: BTreeMap<Slot, f32>,
    universes: BTreeMap<u16, Universe>,
    packets_sent: u64,
    /// `ShowRuntime::dmx_generation` the patch and library were copied at.
    generation: u64,
}

impl DmxView {
    pub fn patch(&self) -> &Patch {
        &self.patch
    }
    pub fn universe(&self, index: u16) -> Option<&Universe> {
        self.universes.get(&index)
    }
    pub fn packets_sent(&self) -> u64 {
        self.packets_sent
    }
    pub fn output(&self, fixture: FixtureId, attribute: &str) -> Option<f32> {
        let r = self.patch.resolve(fixture, attribute)?;
        self.last.get(&Slot { universe: r.universe, channel: r.coarse }).copied()
    }
    pub fn fixture_color(&self, fixture: FixtureId) -> Option<[f32; 3]> {
        livestage_dmx::engine::fixture_color(&self.patch, fixture, |a| self.output(fixture, a))
    }
    pub fn programmer_set(&mut self, attribute: &str, value: f32) {
        self.console.programmer.set_selected(attribute, value, &self.patch);
    }
    pub fn programmer_release(&mut self, attribute: &str) {
        for f in self.console.programmer.selection.clone() {
            self.console.programmer.release(f, attribute);
        }
    }
    pub fn programmer_apply_palette(&mut self, palette: &Palette) -> usize {
        self.console.programmer.apply_palette(palette, &self.patch)
    }
    pub fn programmer_palette_values(&self, kind: PaletteKind) -> Vec<LightValue> {
        self.console.programmer.palette_values(kind, &self.patch)
    }
}

/// Everything the panels read and edit during one frame.
pub struct UiRuntime {
    pub bundle: BundleView,
    /// The project as copied at sync, to tell the operator's edits apart.
    base: Project,
    pub dmx: DmxView,
    pub audio: Option<AudioView>,
    pub audio_error: Option<String>,
    pub timecode: TimecodeView,
    pub timecode_input: String,
    pub midi_in: Option<MidiView>,
    pub log: VecDeque<LogLine>,
    log_seq: u64,
    pub dirty: bool,
    pub undo: UndoView,
    pub outputs_live: bool,
    pub imports: Vec<ImportView>,
    pub office_path: Option<PathBuf>,
    office_path_base: Option<PathBuf>,
    pub restart_control_requested: bool,
    pub show_stats: Arc<ShowStats>,
    hub: ControlHub,
    video_tx: crossbeam_channel::Sender<crate::runtime::VideoMsg>,
    epoch: Instant,
    osc_port: Option<u16>,
    ws_port: Option<u16>,
    /// Audio sample-clock position and end-of-media for active cues.
    media: HashMap<CueId, (Option<ShowTime>, bool)>,
    video_finished: HashSet<CueId>,
    ops: Vec<Op>,
    changed: bool,
    /// Copies released by `sync`, freed outside the lock.
    trash: Vec<Project>,
}

impl UiRuntime {
    /// First copy of a runtime (lock held by the caller).
    pub fn new(rt: &ShowRuntime) -> Self {
        let mut v = Self {
            bundle: BundleView { root: rt.bundle.root().to_path_buf(), project: Project::new("") },
            base: Project::new(""),
            dmx: DmxView {
                console: ConsoleView::default(),
                library: Arc::new(rt.dmx.library.clone()),
                patch_errors: Vec::new(),
                patch: Arc::new(Patch::default()),
                last: BTreeMap::new(),
                universes: BTreeMap::new(),
                packets_sent: 0,
                generation: u64::MAX,
            },
            audio: None,
            audio_error: None,
            timecode: TimecodeView::default(),
            timecode_input: String::new(),
            midi_in: None,
            log: VecDeque::new(),
            log_seq: u64::MAX,
            dirty: false,
            undo: UndoView::default(),
            outputs_live: false,
            imports: Vec::new(),
            office_path: None,
            office_path_base: None,
            restart_control_requested: false,
            show_stats: rt.show_stats.clone(),
            hub: rt.hub.clone(),
            video_tx: rt.video_sender(),
            epoch: rt.epoch(),
            osc_port: None,
            ws_port: None,
            media: HashMap::new(),
            video_finished: HashSet::new(),
            ops: Vec::new(),
            changed: false,
            trash: Vec::new(),
        };
        v.sync(rt);
        v
    }

    /// Step 1: copies the live state (call with the runtime lock held).
    pub fn sync(&mut self, rt: &ShowRuntime) {
        // Freeing last frame's copy costs as much as making the new one, so
        // it is parked and dropped in `begin_frame`, outside the lock.
        let previous = std::mem::replace(&mut self.base, rt.bundle.project.clone());
        self.trash.push(previous);
        if self.bundle.root != rt.bundle.root() {
            self.bundle.root = rt.bundle.root().to_path_buf();
        }

        let d = &mut self.dmx;
        if d.generation != rt.dmx_generation {
            d.generation = rt.dmx_generation;
            d.patch = Arc::new(rt.dmx.patch().clone());
            d.library = Arc::new(rt.dmx.library.clone());
            d.patch_errors.clone_from(&rt.dmx.patch_errors);
        }
        let c = &rt.dmx.console;
        d.console.programmer.clone_from(&c.programmer);
        d.console.flashes.clone_from(&c.flashes);
        d.console.blackout = c.blackout;
        d.console.base = (c.programmer.clone(), c.flashes.clone(), c.blackout);
        d.console.running = rt.bundle.project.lighting.effects.iter().map(|e| e.id).filter(|id| c.effect_running(*id)).collect();
        d.console.console_fx = c.console_effects();
        d.last.clone_from(rt.dmx.last_values());
        d.universes.clear();
        for u in &rt.dmx.config().universes {
            if let Some(x) = rt.dmx.universe(u.index) {
                d.universes.insert(u.index, x.clone());
            }
        }
        d.packets_sent = rt.dmx.packets_sent();

        self.audio = rt.audio.as_ref().map(|a| {
            // Peaks accumulate until read, so frames that skip the meters lose nothing.
            let mut peaks = a.take_peaks();
            if let Some(prev) = self.audio.as_ref().filter(|p| p.peaks.len() == peaks.len()) {
                for (p, q) in peaks.iter_mut().zip(&prev.peaks) {
                    *p = p.max(*q);
                }
            }
            AudioView {
                device_name: a.device_name().to_string(),
                sample_rate: a.sample_rate(),
                channels: a.channels(),
                dsp_load: a.dsp_load(),
                leaked: a.shared().leaked.load(std::sync::atomic::Ordering::Relaxed),
                peaks,
            }
        });
        self.audio_error.clone_from(&rt.audio_error);
        self.media.clear();
        if let Some(a) = &rt.audio {
            for id in active_cues(&self.base) {
                self.media.insert(id, (a.media_position(id), a.media_finished(id)));
            }
        }
        self.video_finished.clone_from(&rt.video_finished.lock());

        self.timecode.config.clone_from(rt.timecode.config());
        self.timecode.transport = rt.timecode.transport();
        self.timecode.lock_error_us = rt.timecode.lock_error_us();
        self.timecode.reset_requested = false;
        self.timecode_input.clone_from(&rt.timecode_input);
        self.midi_in = rt.midi_in.as_ref().map(|m| MidiView { port_name: m.port_name.clone(), learned: m.learn_slot() });

        if self.log_seq != rt.log_seq {
            self.log_seq = rt.log_seq;
            self.log.clone_from(&rt.log);
        }
        self.dirty = rt.dirty;
        self.undo = UndoView { can_undo: rt.undo.can_undo(), can_redo: rt.undo.can_redo() };
        self.outputs_live = rt.outputs_live;
        self.imports = rt.imports.iter().map(|j| ImportView { name: j.name.clone(), progress: j.progress.clone() }).collect();
        self.office_path.clone_from(&rt.office_path);
        self.office_path_base.clone_from(&rt.office_path);
        self.restart_control_requested = false;
        self.osc_port = rt.osc_port();
        self.ws_port = rt.ws_port();
        self.ops.clear();
        self.changed = false;
    }

    /// Step 2 (no lock): the copy the panels edit. Call after [`sync`](Self::sync).
    pub fn begin_frame(&mut self) {
        self.trash.clear();
        self.bundle.project = self.base.clone();
    }

    /// What the panels changed in the project this frame. All comparisons
    /// happen here, outside the lock.
    pub fn take_edits(&mut self) -> Option<ProjectEdits> {
        let ours = std::mem::replace(&mut self.bundle.project, Project::new(""));
        ProjectEdits::diff(&self.base, ours)
    }

    /// Step 3: moves the frame's edits and queued work into the live runtime
    /// (call with the runtime lock held, after [`take_edits`](Self::take_edits)).
    pub fn apply(&mut self, rt: &mut ShowRuntime, edits: Option<ProjectEdits>) {
        if let Some(e) = edits {
            rt.bundle.project.apply_edits(e);
        }
        let c = &mut self.dmx.console;
        if c.programmer != c.base.0 {
            rt.dmx.console.programmer = std::mem::take(&mut c.programmer);
        }
        if c.flashes != c.base.1 {
            rt.dmx.console.flashes = std::mem::take(&mut c.flashes);
        }
        if c.blackout != c.base.2 {
            rt.dmx.console.blackout = c.blackout;
        }
        for (id, running, now) in c.fx_changes.drain(..) {
            rt.dmx.console.set_effect(id, running, now);
        }
        if self.timecode.reset_requested {
            rt.timecode.reset();
        }
        if self.office_path != self.office_path_base {
            rt.office_path = self.office_path.take();
        }
        if self.restart_control_requested {
            rt.restart_control_requested = true;
        }
        if self.changed {
            rt.mark_changed();
        }
        for op in self.ops.drain(..) {
            op(rt);
        }
    }

    fn defer(&mut self, op: impl FnOnce(&mut ShowRuntime) + Send + 'static) {
        self.ops.push(Box::new(op));
    }

    fn ctx(&self) -> ViewCtx<'_> {
        ViewCtx { now: self.now(), media: &self.media, video_finished: &self.video_finished, scratch: Vec::new() }
    }

    // ----------------------------------------------------- engine passthrough --

    pub fn now(&self) -> ShowTime {
        self.epoch.elapsed().into()
    }
    pub fn command(&self, source: ControlSource, cmd: ShowCommand) {
        self.hub.send(source, cmd);
    }
    pub fn send_video(&self, action: VideoAction) {
        let _ = self.video_tx.send((self.now(), action));
    }
    pub fn osc_port(&self) -> Option<u16> {
        self.osc_port
    }
    pub fn ws_port(&self) -> Option<u16> {
        self.ws_port
    }
    pub fn log(&mut self, level: LogLevel, text: impl Into<String>) {
        let text = text.into();
        // Shown at once; the live log gets it after the frame.
        self.log.push_back(LogLine { at: self.now(), text: text.clone(), level });
        self.defer(move |rt| rt.log(level, text));
    }

    // ------------------------------------------------------------ queued work --

    /// Records an edit (dirty flag, undo, autosave, MIDI bindings).
    pub fn mark_changed(&mut self) {
        self.changed = true;
        self.dirty = true;
    }
    pub fn after_structure_edit(&mut self) {
        self.mark_changed();
        self.defer(|rt| rt.after_structure_edit());
    }
    pub fn palettes_edited(&mut self) {
        self.mark_changed();
        self.defer(|rt| rt.palettes_edited());
    }
    pub fn commit_undo_when_idle(&mut self, pointer_down: bool) {
        self.defer(move |rt| rt.undo.maybe_commit(&rt.bundle.project, pointer_down));
    }
    pub fn seek_cue(&mut self, id: CueId, position: ShowTime) {
        self.defer(move |rt| rt.seek_cue(id, position));
    }
    pub fn slide_nav(&mut self, target: Option<CueId>, nav: SlideNav) {
        self.defer(move |rt| {
            rt.slide_nav(target, nav);
        });
    }
    pub fn cue_op(&mut self, id: CueId, op: CueOp) {
        self.defer(move |rt| rt.cue_op(id, op));
    }
    pub fn undo(&mut self) {
        self.defer(|rt| rt.undo());
    }
    pub fn redo(&mut self) {
        self.defer(|rt| rt.redo());
    }
    pub fn save(&mut self) {
        self.dirty = false;
        self.defer(|rt| rt.save());
    }
    pub fn recover_autosave(&mut self) {
        self.defer(|rt| rt.recover_autosave());
    }
    pub fn discard_autosave(&mut self) {
        self.defer(|rt| rt.bundle.discard_autosave());
    }
    pub fn import_and_create_cue(&mut self, path: &Path) {
        let path = path.to_path_buf();
        self.defer(move |rt| rt.import_and_create_cue(&path));
    }
    pub fn rebuild_dmx(&mut self) {
        self.defer(|rt| rt.rebuild_dmx());
    }
    pub fn refresh_midi_bindings(&mut self) {
        self.defer(|rt| rt.refresh_midi_bindings());
    }
    pub fn start_audio(&mut self) {
        self.defer(|rt| rt.start_audio());
    }
    pub fn apply_audio_routing(&mut self) {
        self.defer(|rt| rt.apply_audio_routing());
    }
    pub fn apply_timecode_config(&mut self) {
        self.defer(|rt| rt.apply_timecode_config());
    }
    /// Mutes MIDI triggers (while learning a pad note); `refresh_midi_bindings` restores them.
    pub fn silence_midi_bindings(&mut self) {
        self.defer(|rt| {
            if let Some(m) = &rt.midi_in {
                m.set_bindings(Vec::new());
            }
        });
    }
    pub fn open_midi(&mut self, port: Option<&str>) {
        let port = port.map(str::to_string);
        self.defer(move |rt| rt.open_midi(port.as_deref()));
    }

    // ------------------------------------------------------ queries on the copy --

    pub fn find_cue(&self, id: CueId) -> Option<&AnyCue> {
        let p = &self.bundle.project;
        p.cue_lists.iter().find_map(|l| l.find(id)).or_else(|| p.carts.iter().find_map(|k| k.find(id)))
    }
    pub fn find_cue_mut(&mut self, id: CueId) -> Option<&mut AnyCue> {
        let p = &mut self.bundle.project;
        crate::runtime::find_cue_in(&mut p.cue_lists, &mut p.carts, id)
    }
    pub fn standby_cue(&self) -> Option<&AnyCue> {
        self.bundle.project.active_list()?.standby_cue()
    }
    pub fn current_slides(&self) -> Option<CueId> {
        crate::runtime::current_slides(&self.bundle.project)
    }
    pub fn next_cue_number(&self) -> String {
        crate::runtime::next_cue_number(&self.bundle.project)
    }
    pub fn running_count(&self) -> usize {
        self.bundle.project.active_list().map_or(0, |l| l.cues.iter().filter(|c| c.state().is_active()).count())
    }
    pub fn cue_rows(&self) -> Vec<CueRow> {
        crate::runtime::cue_rows(&self.bundle.project, &self.ctx())
    }
    /// Elapsed time of any active cue (searching all lists and timelines).
    pub fn elapsed_of(&self, id: CueId) -> Option<ShowTime> {
        let cue = self.find_cue(id)?;
        cue.state().is_active().then(|| cue.elapsed(&self.ctx()))
    }
    /// The most recently started running cue: (number, name, elapsed, remaining).
    pub fn running_summary(&self) -> Option<(String, String, ShowTime, Option<ShowTime>)> {
        let (cue, elapsed) = self.program_cue()?;
        let h = cue.header();
        Some((h.number.clone(), h.name.clone(), elapsed, cue.remaining(&self.ctx())))
    }
    /// The program cue (most recently started running cue of the active list) and its elapsed time.
    pub fn program_cue(&self) -> Option<(&AnyCue, ShowTime)> {
        let list = self.bundle.project.active_list()?;
        let cue = list.cues.iter().filter(|c| c.state().is_active()).max_by_key(|c| c.header().runtime.started_at)?;
        Some((cue, cue.elapsed(&self.ctx())))
    }
    pub fn cue_for_asset(&self, rec: &AssetRecord, number: &str, name: &str) -> Option<AnyCue> {
        crate::runtime::cue_for_asset(&self.bundle.root, &self.bundle.project, rec, number, name)
    }

    // ------------------------------------------------------- edits on the copy --

    /// Inserts a copy of a top-level cue right after it (fresh ids, next free number).
    pub fn duplicate_cue(&mut self, id: CueId) -> Option<CueId> {
        let number = self.next_cue_number();
        let list = self.bundle.project.active_list_mut()?;
        let i = list.index_of(id)?;
        let mut copy = list.cues[i].clone();
        renew_ids(&mut copy);
        copy.header_mut().number = number;
        copy.header_mut().name = format!("{} (copy)", copy.header().name);
        let new_id = copy.header().id;
        list.cues.insert(i + 1, copy);
        self.after_structure_edit();
        Some(new_id)
    }

    pub fn add_cue_list(&mut self, name: &str) -> CueListId {
        let mut list = CueList::new(name);
        let id = list.id;
        let mut ctx = self.ctx();
        list.reset(&mut ctx);
        self.bundle.project.cue_lists.push(list);
        self.bundle.project.active_cue_list = Some(id);
        self.mark_changed();
        id
    }

    pub fn add_cart(&mut self, rows: u8, cols: u8) -> usize {
        let n = self.bundle.project.carts.len() + 1;
        self.bundle.project.carts.push(CueCart::new(format!("Cart {n}"), rows, cols));
        self.mark_changed();
        n - 1
    }

    /// Puts `cue` on a pad (keeping the pad's mode, colour and bindings if
    /// it already had them). The cue gets fresh ids and a pad number.
    pub fn set_pad_cue(&mut self, cart: usize, slot: usize, mut cue: AnyCue) -> Option<CueId> {
        let k = self.bundle.project.carts.get_mut(cart)?;
        let old = k.pads.get_mut(slot)?;
        renew_ids(&mut cue);
        cue.header_mut().number = format!("C{}.{}", cart + 1, slot + 1);
        cue.header_mut().pre_wait = ShowTime::ZERO;
        cue.header_mut().follow = livestage_core::cue::FollowMode::None;
        cue.header_mut().timecode = None;
        let id = cue.header().id;
        match old {
            Some(p) if !p.is_playing() => p.cue = cue,
            Some(_) => return None, // never swap a playing pad's cue
            None => *old = Some(livestage_core::cart::CartPad::new(cue)),
        }
        self.mark_changed();
        Some(id)
    }
}

/// Ids of every active cue (lists, timeline children, cart pads).
fn active_cues(p: &Project) -> Vec<CueId> {
    fn walk(c: &AnyCue, out: &mut Vec<CueId>) {
        if c.state().is_active() {
            out.push(c.header().id);
            if let AnyCue::Timeline(t) = c {
                if let Some(m) = &t.master {
                    walk(m, out);
                }
                for i in &t.items {
                    walk(&i.cue, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    for c in p.cue_lists.iter().flat_map(|l| &l.cues).chain(p.carts.iter().flat_map(|k| k.pads.iter().flatten().map(|p| &p.cue))) {
        walk(c, &mut out);
    }
    out
}
