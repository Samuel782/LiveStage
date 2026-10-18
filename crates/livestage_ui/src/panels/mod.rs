//! Operator UI: transport bar + dockable panels.

mod audio;
mod control;
mod cue_grid;
mod displays;
mod dmx;
mod inspector;
mod multiview;
pub mod settings;
mod timeline;
mod transport;
pub mod widgets;

use crate::runtime::ShowRuntime;
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};
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
    Displays,
    AudioPatch,
    Dmx,
    Control,
}

impl Tab {
    pub const ALL: [Tab; 8] =
        [Tab::CueList, Tab::Inspector, Tab::Timeline, Tab::Multiview, Tab::Displays, Tab::AudioPatch, Tab::Dmx, Tab::Control];

    pub fn title(self) -> &'static str {
        match self {
            Tab::CueList => "Cue List",
            Tab::Inspector => "Inspector",
            Tab::Timeline => "Timeline",
            Tab::Multiview => "Multiview",
            Tab::Displays => "Display Routing",
            Tab::AudioPatch => "Audio Patch",
            Tab::Dmx => "DMX",
            Tab::Control => "Control & Log",
        }
    }
}

/// Show-control layout: cue list top-left, inspector top-right, everything
/// else as tabs along the bottom.
fn default_dock() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::CueList]);
    let tree = dock.main_surface_mut();
    let [top, _bottom] = tree.split_below(
        NodeIndex::root(),
        0.64,
        vec![Tab::Timeline, Tab::Multiview, Tab::Displays, Tab::AudioPatch, Tab::Dmx, Tab::Control],
    );
    tree.split_right(top, 0.63, vec![Tab::Inspector]);
    dock
}

pub struct UiState {
    pub dock: DockState<Tab>,
    pub selected: Option<CueId>,
    pub import_path: String,
    pub meters: Vec<f32>,
    pub audio_devices: Vec<livestage_audio::OutputDeviceInfo>,
    pub midi_inputs: Vec<String>,
    pub test_patterns: std::collections::HashSet<OutputId>,
    pub dragging_warp: Option<(OutputId, usize)>,
    pub new_fixture_def: String,
    /// Show-thread health shown in the status bar: (sampled at, tick count, text).
    pub health: (Option<std::time::Instant>, u64, String),
    /// Set when an autosave newer than the saved show was found at startup.
    pub recovery: Option<String>,
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
            test_patterns: Default::default(),
            dragging_warp: None,
            new_fixture_def: "generic/rgb-par".into(),
            health: (None, 0, String::new()),
            recovery: None,
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
            self.dock.set_active_tab(loc);
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
    rt: &'a mut ShowRuntime,
    st: &'a mut UiState,
    physical: &'a [PhysicalDisplayInfo],
    previews: &'a Previews,
    ndi: &'a NdiStatus,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title().into()
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::CueList => cue_grid::show(ui, self.rt, self.st),
            Tab::Inspector => inspector::show(ui, self.rt, self.st),
            Tab::Timeline => timeline::show(ui, self.rt, self.st),
            Tab::Multiview => multiview::show(ui, self.rt, self.st, self.previews, self.ndi),
            Tab::Displays => displays::show(ui, self.rt, self.st, self.physical, self.previews, self.ndi),
            Tab::AudioPatch => audio::show(ui, self.rt, self.st),
            Tab::Dmx => dmx::show(ui, self.rt, self.st),
            Tab::Control => control::show(ui, self.rt, self.st),
        }
    }
    fn closeable(&mut self, _tab: &mut Tab) -> bool {
        false
    }
}

fn shortcuts(ctx: &egui::Context, rt: &mut ShowRuntime, st: &mut UiState) {
    // Settings opens even while a text field has focus (as on macOS).
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
        st.settings_open = !st.settings_open;
    }
    if ctx.wants_keyboard_input() {
        return;
    }
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
        if let Some(id) = selected_top {
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
        if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
            rt.command(src(), ShowCommand::StandbyNext);
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
            rt.command(src(), ShowCommand::StandbyPrevious);
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

fn quit_modal(ctx: &egui::Context, st: &mut UiState) {
    if !st.confirm_quit {
        return;
    }
    egui::Modal::new(egui::Id::new("confirm-quit")).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.heading("Quit while outputs are live?");
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

fn recovery_modal(ctx: &egui::Context, rt: &mut ShowRuntime, st: &mut UiState) {
    let Some(when) = st.recovery.clone() else { return };
    egui::Modal::new(egui::Id::new("recovery")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("Recover unsaved changes?");
        ui.label(format!("LiveStage found an autosave from {when} that is newer than the saved show. It was probably left by a crash or a forced quit."));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(egui::RichText::new("Recover").strong()).clicked() {
                rt.recover_autosave();
                st.recovery = None;
            }
            if ui.button("Discard autosave").clicked() {
                rt.bundle.discard_autosave();
                st.recovery = None;
            }
        });
    });
}

pub fn draw(
    ctx: &egui::Context,
    rt: &mut ShowRuntime,
    st: &mut UiState,
    physical: &[PhysicalDisplayInfo],
    previews: &Previews,
    ndi: &NdiStatus,
) {
    st.thumbs.poll(ctx);
    shortcuts(ctx, rt, st);
    let pointer_down = ctx.input(|i| i.pointer.any_down());
    {
        let current = &rt.bundle.project;
        rt.undo.maybe_commit(current, pointer_down);
    }
    recovery_modal(ctx, rt, st);
    quit_modal(ctx, st);
    settings::window(ctx, rt, st);

    // Files dropped on the window are imported into the bundle as cues.
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
    for p in dropped {
        rt.import_and_create_cue(&p);
    }

    egui::TopBottomPanel::top("menu")
        .frame(egui::Frame::default().fill(crate::theme::BG).inner_margin(egui::Margin::symmetric(8, 2)))
        .show(ctx, |ui| transport::menu_bar(ui, rt, st));
    egui::TopBottomPanel::top("transport")
        .exact_height(transport::HEIGHT + 20.0)
        .frame(egui::Frame::default().fill(crate::theme::PANEL_RECESSED).inner_margin(10.0))
        .show(ctx, |ui| transport::show(ui, rt, st));
    egui::TopBottomPanel::bottom("status")
        .exact_height(26.0)
        .frame(egui::Frame::default().fill(crate::theme::PANEL_RECESSED).inner_margin(egui::Margin::symmetric(10, 2)))
        .show(ctx, |ui| transport::status_bar(ui, rt, st));
    transport::import_window(ctx, rt, st);
    egui::CentralPanel::default().frame(egui::Frame::default().fill(crate::theme::BG).inner_margin(4.0)).show(ctx, |ui| {
        let style = crate::theme::dock_style(ui.style());
        let mut dock = std::mem::replace(&mut st.dock, DockState::new(vec![]));
        DockArea::new(&mut dock)
            .style(style)
            .show_close_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_inside(ui, &mut Viewer { rt, st, physical, previews, ndi });
        st.dock = dock;
    });
}
