//! Operator UI: a fixed header (control deck), a dockable workspace (cue
//! list, inspector and tools, all `egui_dock` tabs) and a fixed footer
//! (status lights and the operator log).

mod audio;
pub mod cart;
pub mod lighting;
mod control;
mod cue_grid;
mod displays;
mod dmx;
mod footer;
mod header;
pub mod inspector;
mod multiview;
pub mod settings;
mod timeline;
mod transport;
pub(crate) use crate::widgets;

use crate::view::UiRuntime;
use egui_dock::{DockArea, DockState, TabViewer};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::id::CueId;
use livestage_core::routing::PhysicalDisplayInfo;
use livestage_core::OutputId;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    CueList,
    Inspector,
    Timeline,
    Multiview,
    Carts,
    Lighting,
    Displays,
    AudioPatch,
    Dmx,
    Control,
}

impl Tab {
    pub const ALL: [Tab; 10] = [
        Tab::CueList,
        Tab::Inspector,
        Tab::Timeline,
        Tab::Multiview,
        Tab::Carts,
        Tab::Lighting,
        Tab::Displays,
        Tab::AudioPatch,
        Tab::Dmx,
        Tab::Control,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::CueList => "Cue List",
            Tab::Inspector => "Inspector",
            Tab::Timeline => "Timeline",
            Tab::Multiview => "Multiview",
            Tab::Carts => "Soundboard",
            Tab::Lighting => "Lighting",
            Tab::Displays => "Display Routing",
            Tab::AudioPatch => "Audio Mixer",
            Tab::Dmx => "DMX",
            Tab::Control => "Control & Log",
        }
    }
}

/// Default workspace: the cue list centre stage, the inspector on its
/// right, and the tools in a tab group along the bottom. Every tab can be
/// dragged, split and re-arranged (View ▸ Reset layout restores this).
fn default_dock() -> DockState<Tab> {
    use egui_dock::NodeIndex;
    let mut dock = DockState::new(vec![Tab::CueList]);
    let surface = dock.main_surface_mut();
    let tools = vec![Tab::Timeline, Tab::Multiview, Tab::Carts, Tab::Lighting, Tab::Displays, Tab::AudioPatch, Tab::Dmx, Tab::Control];
    let [top, _tools] = surface.split_below(NodeIndex::root(), 0.58, tools);
    surface.split_right(top, 0.70, vec![Tab::Inspector]);
    dock
}

/// A/V lock telemetry: timeline video layers following an audio master.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AvSync {
    pub layers: u64,
    /// Worst audio/video offset over the last second, milliseconds.
    pub worst_ms: f32,
    /// Hard re-times since the outputs started.
    pub snaps: u64,
}

impl AvSync {
    /// Within a frame at 50 fps: invisible on stage.
    pub fn tight(&self) -> bool {
        self.worst_ms < 20.0
    }
}

/// Watchdog findings (see [`crate::health`]) for the status bar.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WatchView {
    pub show_stalled: bool,
    pub output_stalled: bool,
    pub audio_stalled: bool,
    /// Show / output stalls and contained faults since launch.
    pub stalls: u64,
    pub faults: u64,
    /// Consecutive failed autosaves.
    pub autosave_failures: u64,
}

impl WatchView {
    /// Something is wrong right now.
    pub fn alarm(&self) -> bool {
        self.show_stalled || self.output_stalled || self.audio_stalled || self.autosave_failures > 0
    }
    /// Worth showing: wrong now, or went wrong earlier this session.
    pub fn notable(&self) -> bool {
        self.alarm() || self.stalls > 0 || self.faults > 0
    }
}

pub struct UiState {
    pub dock: DockState<Tab>,
    pub selected: Option<CueId>,
    pub import_path: String,
    pub meters: Vec<widgets::meter::MeterState>,
    pub audio_devices: Vec<livestage_audio::OutputDeviceInfo>,
    pub midi_inputs: Vec<String>,
    pub midi_outputs: Vec<String>,
    /// Remote token being issued: client label, validity, and the result.
    pub token_label: String,
    pub token_days: u32,
    pub issued_token: Option<String>,
    /// Art-Net timecode destination being typed (applied on Enter / focus loss).
    pub artnet_tc_dest: Option<String>,
    /// Audio inputs (name, channels), for LTC.
    pub audio_inputs: Vec<(String, u16)>,
    /// Fixture file to import (GDTF / OFL), and the last import's result.
    pub fixture_import_path: String,
    pub fixture_import_result: Option<(bool, String)>,
    pub test_patterns: std::collections::HashSet<OutputId>,
    pub dragging_warp: Option<(OutputId, usize)>,
    pub new_fixture_def: String,
    /// Show-thread health for the deck's system module, sampled once a second.
    pub health: transport::Health,
    /// Set when an autosave newer than the saved show was found at startup.
    pub recovery: Option<String>,
    /// Set when the last session ended without a clean quit.
    pub unclean_exit: Option<String>,
    /// Timeline editor zoom (pixels per second) and snap.
    pub tl_zoom: f32,
    pub tl_snap: bool,
    pub tl_drag: Option<(livestage_core::id::CueId, f32)>,
    pub renaming_list: Option<(livestage_core::id::CueListId, String)>,
    pub import_open: bool,
    /// Decoded thumbnails (slides, stills, movie poster frames).
    pub thumbs: crate::thumbs::Thumbs,
    pub settings_open: bool,
    pub settings_section: settings::Section,
    pub prefs: crate::prefs::Preferences,
    /// Quit was requested while outputs are live: ask first.
    pub confirm_quit: bool,
    /// The operator confirmed quitting (handled by the app).
    pub quit_requested: bool,
    /// Output render thread frame rate.
    pub output_fps: f32,
    /// Video layers frame-locked to audio, as published by the output thread.
    pub av_sync: AvSync,
    pub watch: WatchView,
    /// Soundboard: visible cart, edit mode, pad being edited, pad held by
    /// the pointer, and pending key / MIDI learn.
    pub cart_page: usize,
    pub cart_edit: bool,
    pub cart_slot: Option<usize>,
    pub pad_pointer: Option<(usize, usize)>,
    pub pad_learn: Option<cart::PadLearn>,
    /// Lighting console: encoder / palette category, stage plot editing,
    /// and the submaster whose flash button the pointer holds.
    pub light_tab: livestage_core::lighting::PaletteKind,
    pub light_move: bool,
    pub light_drag: Option<lighting::StageDrag>,
    pub light_flash: Option<livestage_core::id::SubmasterId>,
    /// The operator closed the movie-playback warning banner.
    pub media_banner_dismissed: bool,
    /// The deck has room for the system module (else the status bar shows health).
    pub deck_shows_system: bool,
    /// Live mode: editing is locked (inspector, structure, timeline drags,
    /// imports); only show operation works.
    pub live_mode: bool,
    /// Inspector page.
    pub inspector_tab: inspector::Page,
    /// Operator log drawer open, and its level filter.
    pub log_open: bool,
    pub log_filter: Option<crate::runtime::LogLevel>,
    /// Status-bar activity lights.
    pub activity: footer::Activity,
    /// Output channel meters and the master (loudest channel) meter,
    /// fed once per frame from the engine's peaks.
    pub master_meter: widgets::meter::MeterState,
    pub meter_clock: Option<f64>,
    /// Cue list keyboard selection: the row to scroll into view, and
    /// whether the selection should follow the next standby change.
    pub grid_scroll_to: Option<CueId>,
    pub select_follows_standby: bool,
    /// Rows to move the selection by (Shift+↑/↓), applied by the cue list.
    pub select_step: i32,
    /// Mixer: stems soloed (audition only, not saved) and the routing matrix window.
    pub soloed: std::collections::HashSet<livestage_core::id::StemId>,
    pub routing_open: bool,
    /// Display mapping: output being placed on a screen, editor mode, the
    /// mask being edited and the mask point being dragged.
    pub display_assign: Option<OutputId>,
    pub map_mode: displays::MapMode,
    pub mask_sel: Option<usize>,
    pub dragging_mask: Option<(OutputId, usize, usize)>,
    pub last_standby: Option<CueId>,
    /// The About LiveStage box is open.
    pub about_open: bool,
}

impl Default for UiState {
    fn default() -> Self {
        let dock = default_dock();
        Self {
            dock,
            selected: None,
            import_path: String::new(),
            meters: Vec::new(),
            audio_devices: Vec::new(),
            midi_inputs: Vec::new(),
            midi_outputs: Vec::new(),
            token_label: "remote".into(),
            artnet_tc_dest: None,
            token_days: 30,
            issued_token: None,
            audio_inputs: Vec::new(),
            fixture_import_path: String::new(),
            fixture_import_result: None,
            test_patterns: Default::default(),
            dragging_warp: None,
            new_fixture_def: "generic/rgb-par".into(),
            health: Default::default(),
            recovery: None,
            unclean_exit: None,
            tl_zoom: 40.0,
            tl_snap: true,
            tl_drag: None,
            renaming_list: None,
            import_open: false,
            thumbs: Default::default(),
            settings_open: false,
            settings_section: Default::default(),
            prefs: Default::default(),
            confirm_quit: false,
            quit_requested: false,
            output_fps: 0.0,
            av_sync: AvSync::default(),
            watch: WatchView::default(),
            cart_page: 0,
            cart_edit: false,
            cart_slot: None,
            pad_pointer: None,
            pad_learn: None,
            light_tab: livestage_core::lighting::PaletteKind::Color,
            light_move: false,
            light_drag: None,
            light_flash: None,
            media_banner_dismissed: false,
            deck_shows_system: false,
            live_mode: false,
            inspector_tab: inspector::Page::General,
            log_open: false,
            log_filter: None,
            activity: Default::default(),
            master_meter: Default::default(),
            meter_clock: None,
            grid_scroll_to: None,
            select_follows_standby: false,
            select_step: 0,
            soloed: Default::default(),
            routing_open: false,
            display_assign: None,
            map_mode: Default::default(),
            mask_sel: None,
            dragging_mask: None,
            last_standby: None,
            about_open: false,
        }
    }
}

impl UiState {
    pub fn reset_layout(&mut self) {
        self.dock = default_dock();
    }

    /// Brings a tab to the front, re-adding it if it was closed.
    pub fn focus_tab(&mut self, tab: Tab) {
        if let Some(loc) = self.dock.find_tab(&tab) {
            // `loc` was just found, so it is valid.
            let _ = self.dock.set_active_tab(loc);
        } else {
            self.dock.push_to_focused_leaf(tab);
        }
    }
}

/// Live canvas previews registered with the control window's egui renderer.
pub type Previews = HashMap<OutputId, (egui::TextureId, (u32, u32))>;

/// Live NDI publication state per output: receivers connected.
pub type NdiStatus = HashMap<OutputId, i32>;

struct Viewer<'a> {
    rt: &'a mut UiRuntime,
    st: &'a mut UiState,
    physical: &'a [PhysicalDisplayInfo],
    previews: &'a Previews,
    ndi: &'a NdiStatus,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;
    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(tab.title())
    }
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        // Title case at callout size, as macOS tab views label their tabs.
        crate::theme::styled(tab.title(), crate::theme::type_scale::CALLOUT + 0.5).into()
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::CueList => cue_grid::show(ui, self.rt, self.st),
            Tab::Inspector => inspector_tab(ui, self.rt, self.st),
            Tab::Timeline => timeline::show(ui, self.rt, self.st),
            Tab::Multiview => multiview::show(ui, self.rt, self.st, self.previews, self.ndi),
            Tab::Carts => cart::show(ui, self.rt, self.st),
            Tab::Lighting => lighting::show(ui, self.rt, self.st),
            Tab::Displays => displays::show(ui, self.rt, self.st, self.physical, self.previews, self.ndi),
            Tab::AudioPatch => audio::show(ui, self.rt, self.st),
            Tab::Dmx => dmx::show(ui, self.rt, self.st),
            Tab::Control => control::show(ui, self.rt, self.st),
        }
    }
    fn closeable(&mut self, _tab: &mut Tab) -> bool {
        false
    }
    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        // These manage their own scrolling (sticky table header, canvases).
        match tab {
            Tab::CueList | Tab::Inspector | Tab::Timeline | Tab::Multiview | Tab::Displays | Tab::Lighting | Tab::Carts => [false, false],
            _ => [true, true],
        }
    }
}

fn shortcuts(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
    // Settings opens even while a text field has focus (as on macOS).
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
        st.settings_open = !st.settings_open;
    }
    if ctx.egui_wants_keyboard_input() || st.pad_learn.is_some() {
        return;
    }
    // Pad keys first, so a pad bound to a key never also triggers a shortcut.
    cart::key_triggers(ctx, rt, st);
    let src = || ControlSource::Keyboard;
    let selected_top = st.selected.filter(|id| rt.bundle.project.active_list().and_then(|l| l.index_of(*id)).is_some());
    ctx.input_mut(|i| {
        use egui::{Key, Modifiers};
        // Presentations: arrows / clicker keys step the running slide cue.
        if i.consume_key(Modifiers::NONE, Key::ArrowRight) || i.consume_key(Modifiers::NONE, Key::PageDown) {
            rt.command(src(), ShowCommand::NextSlide);
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowLeft) || i.consume_key(Modifiers::NONE, Key::PageUp) {
            rt.command(src(), ShowCommand::PrevSlide);
        }
        if let Some(id) = selected_top.filter(|_| !st.live_mode) {
            if i.consume_key(Modifiers::COMMAND, Key::D) {
                st.selected = rt.duplicate_cue(id).or(st.selected);
            }
            if i.consume_key(Modifiers::COMMAND, Key::Backspace) || i.consume_key(Modifiers::COMMAND, Key::Delete) {
                if let Some(l) = rt.bundle.project.active_list_mut() {
                    l.cues.retain(|c| livestage_core::cue::Cue::header(c).id != id);
                }
                st.selected = None;
                rt.after_structure_edit();
            }
        }
        if i.consume_key(Modifiers::NONE, Key::Space) {
            rt.command(src(), ShowCommand::Go { list: None });
        }
        if i.consume_key(Modifiers::NONE, Key::Escape) {
            rt.command(src(), ShowCommand::Panic { hard: false });
        }
        // Shift first: egui's matching ignores an extra Shift, so the plain
        // arrows below would take these too.
        if i.consume_key(Modifiers::SHIFT, Key::ArrowDown) {
            st.select_step += 1;
        }
        if i.consume_key(Modifiers::SHIFT, Key::ArrowUp) {
            st.select_step -= 1;
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
            rt.command(src(), ShowCommand::StandbyNext);
            st.select_follows_standby = true;
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
            rt.command(src(), ShowCommand::StandbyPrevious);
            st.select_follows_standby = true;
        }
        if i.consume_key(Modifiers::NONE, Key::P) {
            rt.command(src(), ShowCommand::PauseAll);
        }
        if i.consume_key(Modifiers::NONE, Key::R) {
            rt.command(src(), ShowCommand::ResumeAll);
        }
        if i.consume_key(Modifiers::COMMAND, Key::I) {
            st.import_open = true;
        }
        if i.consume_key(Modifiers::COMMAND, Key::S) {
            rt.save();
        }
        if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) {
            rt.redo();
        }
        if i.consume_key(Modifiers::COMMAND, Key::Z) {
            rt.undo();
        }
    });
}

/// Title row of an alert: the app icon beside the heading (as macOS and
/// Windows alerts do).
fn alert_heading(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        crate::assets::icon_image(ui, 40.0);
        ui.add_space(4.0);
        ui.label(crate::theme::styled(title, crate::theme::type_scale::HEADING).strong());
    });
    ui.add_space(4.0);
}

fn about_modal(ctx: &egui::Context, st: &mut UiState) {
    if !st.about_open {
        return;
    }
    let r = modal(ctx, "about").show(ctx, |ui| {
        ui.set_width(360.0);
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            crate::assets::icon_image(ui, 112.0);
            ui.add_space(6.0);
            ui.label(crate::theme::styled("LiveStage", crate::theme::type_scale::TITLE).strong());
            ui.label(egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(crate::theme::text_dim()));
            ui.add_space(8.0);
            ui.label("Show control for audio, video, lighting and timecode.");
            let movies = livestage_video::capabilities::movie_support();
            ui.label(egui::RichText::new(movies.summary()).small().color(crate::theme::text_dim()));
            ui.add_space(10.0);
            if ui.button("Close").clicked() {
                st.about_open = false;
            }
        });
    });
    if r.should_close() {
        st.about_open = false;
    }
}

fn quit_modal(ctx: &egui::Context, st: &mut UiState) {
    if !st.confirm_quit {
        return;
    }
    modal(ctx, "confirm-quit").show(ctx, |ui| {
        ui.set_width(380.0);
        alert_heading(ui, "Quit while outputs are live?");
        ui.label("Program outputs are on air. Quitting closes them and stops every cue.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(egui::RichText::new("Quit LiveStage").color(crate::theme::stop()).strong()).clicked() {
                st.quit_requested = true;
                st.confirm_quit = false;
            }
            if ui.button("Cancel").clicked() {
                st.confirm_quit = false;
            }
        });
    });
}

fn recovery_modal(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
    let Some(when) = st.recovery.clone() else { return };
    modal(ctx, "recovery").show(ctx, |ui| {
        ui.set_width(420.0);
        alert_heading(ui, "Recover unsaved changes?");
        match &st.unclean_exit {
            Some(exit) => ui.label(format!("{exit} LiveStage found an autosave from {when} with changes that were never saved.")),
            None => ui.label(format!("LiveStage found an autosave from {when} that is newer than the saved show. It was probably left by a crash or a forced quit.")),
        };
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(egui::RichText::new("Recover").strong()).clicked() {
                rt.recover_autosave();
                st.recovery = None;
            }
            if ui.button("Discard autosave").clicked() {
                rt.discard_autosave();
                st.recovery = None;
            }
        });
    });
}

/// Non-blocking strip under the transport when movie playback is missing or
/// degraded (see [`livestage_video::capabilities`]).
fn media_banner(root: &mut egui::Ui, st: &mut UiState) {
    if st.media_banner_dismissed {
        return;
    }
    let Some(warning) = livestage_video::capabilities::movie_support().warning() else { return };
    egui::Panel::top("media-warning")
        .frame(
            egui::Frame::default()
                .fill(crate::theme::wash(crate::theme::standby(), 0.08))
                .stroke(egui::Stroke::new(1.0_f32, crate::theme::wash(crate::theme::standby(), 0.35)))
                .inner_margin(egui::Margin::symmetric(12, 5)),
        )
        .show(root, |ui| {
            ui.horizontal(|ui| {
                ui.label(widgets::caption_text("Advisory").color(crate::theme::standby()));
                ui.add(egui::Label::new(egui::RichText::new(warning).color(crate::theme::standby())).wrap());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Dismiss").clicked() {
                        st.media_banner_dismissed = true;
                    }
                    if ui.button("Details").clicked() {
                        st.settings_open = true;
                        st.settings_section = settings::Section::Outputs;
                    }
                });
            });
        });
}

/// The inspector tab: a context header (cue type, number, name, state)
/// over the paged editor. Locked in Live mode.
fn inspector_tab(ui: &mut egui::Ui, rt: &mut UiRuntime, st: &mut UiState) {
    use crate::theme;
    use livestage_core::cue::{Cue, CueExt};
    let context = st.selected.and_then(|id| rt.find_cue(id)).map(|c| (c.kind(), c.header().number.clone(), c.header().name.clone(), c.state()));
    match &context {
        Some((kind, number, name, state)) => {
            let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
            let p = ui.painter();
            widgets::badge(p, row.left_center(), kind.label(), theme::kind_color(*kind));
            if let Some(c) = theme::state_color(*state) {
                let g = p.layout_no_wrap(state.label().to_uppercase(), egui::FontId::proportional(10.0), c);
                widgets::pill(p, egui::pos2(row.right() - g.size().x - 16.0, row.center().y), state.label(), c);
            }
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(number).font(theme::clock_font(20.0)).color(theme::text()));
                ui.add(egui::Label::new(egui::RichText::new(name).size(theme::type_scale::HEADING).strong()).truncate());
            });
            if st.live_mode {
                ui.label(egui::RichText::new("Live mode: editing is locked. Switch to Edit in the header to change cues.").size(theme::type_scale::SMALL).color(theme::stop_text()));
            }
            ui.add_space(4.0);
            ui.add_enabled_ui(!st.live_mode, |ui| inspector::show(ui, rt, st));
        }
        None => {
            ui.add_space(theme::GRID * 2.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("No cue selected").size(theme::type_scale::SUBHEAD).color(theme::text_dim()));
                ui.label(egui::RichText::new("Click a row in the cue list to inspect and edit it.").color(theme::text_faint()));
            });
        }
    }
}

/// Feeds the engine's peaks to the meters once per frame (the engine
/// resets its peaks when read, so only this function reads them).
fn sample_meters(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
    let now = ctx.input(|i| i.time);
    let dt = st.meter_clock.map_or(1.0 / 60.0, |t| (now - t) as f32);
    st.meter_clock = Some(now);
    let peaks = rt.audio.as_mut().map(|a| a.take_peaks()).unwrap_or_default();
    if st.meters.len() < peaks.len() {
        st.meters.resize(peaks.len(), Default::default());
    }
    let mut loudest = 0.0f32;
    for (i, m) in st.meters.iter_mut().enumerate() {
        let p = peaks.get(i).copied().unwrap_or(0.0);
        loudest = loudest.max(p);
        m.feed(p, now, dt);
    }
    st.master_meter.feed(loudest, now, dt);
    if rt.audio.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Lays out the whole operator window inside egui's root `ui`.
pub fn draw(
    root: &mut egui::Ui,
    rt: &mut UiRuntime,
    st: &mut UiState,
    physical: &[PhysicalDisplayInfo],
    previews: &Previews,
    ndi: &NdiStatus,
) {
    let ctx = &root.ctx().clone();
    crate::theme::sync(ctx);
    st.thumbs.poll(ctx);
    shortcuts(ctx, rt, st);
    // A pad held with the mouse is released even if its panel was hidden meanwhile.
    if let Some((cart, pad)) = st.pad_pointer {
        if !ctx.input(|i| i.pointer.primary_down()) {
            rt.command(ControlSource::Ui, ShowCommand::CartPad { cart: cart + 1, pad: pad + 1, down: false });
            st.pad_pointer = None;
        }
    }
    // Likewise a submaster flash button.
    if let Some(id) = st.light_flash {
        if !ctx.input(|i| i.pointer.primary_down()) {
            rt.dmx.console.flashes.remove(&id);
            st.light_flash = None;
        }
    }
    let pointer_down = ctx.input(|i| i.pointer.any_down());
    rt.commit_undo_when_idle(pointer_down);
    recovery_modal(ctx, rt, st);
    quit_modal(ctx, st);
    about_modal(ctx, st);
    settings::window(ctx, rt, st);

    // Files dropped on the window are imported into the bundle as cues
    // (not in Live mode).
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
    for p in dropped {
        if st.live_mode {
            rt.log(crate::runtime::LogLevel::Warn, format!("not imported (Live mode): {}", p.display()));
        } else {
            rt.import_and_create_cue(&p);
        }
    }
    sample_meters(ctx, rt, st);
    cart::poll_learn(ctx, rt, st);

    use crate::theme;
    let header_rect = egui::Panel::top("header")
        .exact_size(header::HEIGHT + 2.0 * theme::GRID)
        .show_separator_line(false)
        .frame(egui::Frame::default().fill(theme::bg_deep()).inner_margin(egui::Margin::same(theme::GRID as i8)))
        .show(root, |ui| header::show(ui, rt, st))
        .response
        .rect;
    media_banner(root, st);
    let footer_rect = egui::Panel::bottom("status")
        .exact_size(footer::HEIGHT)
        .show_separator_line(false)
        .frame(egui::Frame::default().fill(theme::bg_deep()).inner_margin(egui::Margin::symmetric(theme::GRID as i8 + 2, 0)))
        .show(root, |ui| footer::status_bar(ui, rt, st))
        .response
        .rect;
    if st.log_open {
        egui::Panel::bottom("log-drawer")
            .resizable(true)
            .default_size(220.0)
            .size_range(120.0..=520.0)
            .show_separator_line(false)
            .frame(egui::Frame::default().fill(theme::raised()).stroke(theme::hairline()).inner_margin(egui::Margin::same(theme::GRID as i8)))
            .show(root, |ui| footer::log_drawer(ui, rt, st));
    }
    transport::import_window(ctx, rt, st);
    // Workspace: every panel is a dock tab (cue list, inspector, tools).
    egui::CentralPanel::default().frame(egui::Frame::default().fill(theme::bg()).inner_margin(theme::GRID * 0.75)).show(root, |ui| {
        let style = theme::dock_style(ui.style());
        let mut dock = std::mem::replace(&mut st.dock, DockState::new(vec![]));
        DockArea::new(&mut dock)
            .id(egui::Id::new("workspace-dock"))
            .style(style)
            .show_close_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_inside(ui, &mut Viewer { rt, st, physical, previews, ndi });
        st.dock = dock;
    });
    // Floating chrome meets the workspace with a soft shadow, not a line.
    // Painted last on the panels' layer, so above the workspace, below windows.
    let edges = root.painter().clone();
    theme::edge_shadow(&edges, header_rect, true, 1.0, 7.0);
    theme::edge_shadow(&edges, footer_rect, true, -1.0, 4.0);
}

/// A modal task: dims the window behind it (cross-fading in) so the task
/// has focus, after the macOS sheet / alert.
pub(crate) fn modal(ctx: &egui::Context, id: &str) -> egui::Modal {
    let id = egui::Id::new(id);
    let dim = crate::motion::fade(ctx, id.with("scrim"), 1.0, crate::motion::Spring::SMOOTH);
    let start = id.with("scrim-start");
    // (Never call into `ctx` inside its own `data` lock.)
    let pass = ctx.cumulative_pass_nr();
    let last = ctx.data(|d| d.get_temp::<u64>(start));
    let shown_last = last.is_some_and(|f| f + 1 >= pass);
    if !shown_last {
        crate::motion::snap(ctx, id.with("scrim"), 0.0);
    }
    ctx.data_mut(|d| d.insert_temp(start, pass));
    egui::Modal::new(id).backdrop_color(crate::theme::modal_scrim().gamma_multiply(if shown_last { dim } else { 0.0 }))
}
