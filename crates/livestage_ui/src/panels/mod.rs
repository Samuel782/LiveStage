//! Operator UI: transport bar + dockable panels.

mod audio;
pub mod cart;
pub mod lighting;
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

use crate::view::UiRuntime;
use egui_dock::{DockArea, DockState, TabViewer};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::id::CueId;
use livestage_core::routing::PhysicalDisplayInfo;
use livestage_core::OutputId;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
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
    pub const ALL: [Tab; 8] = [
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
            Tab::Timeline => "Timeline",
            Tab::Multiview => "Multiview",
            Tab::Carts => "Soundboard",
            Tab::Lighting => "Lighting",
            Tab::Displays => "Display Routing",
            Tab::AudioPatch => "Audio Patch",
            Tab::Dmx => "DMX",
            Tab::Control => "Control & Log",
        }
    }
}

/// Tool dock (the bottom workspace): every tool as a tab, splittable and
/// re-arrangeable. The cue grid and the inspector sit above it and are
/// not tabs.
fn default_dock() -> DockState<Tab> {
    DockState::new(vec![Tab::Timeline, Tab::Multiview, Tab::Carts, Tab::Lighting, Tab::Displays, Tab::AudioPatch, Tab::Dmx, Tab::Control])
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
    /// Show-thread health for the deck's system module, sampled once a second.
    pub health: transport::Health,
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
    /// Inspector side panel expanded (else a thin rail).
    pub inspector_open: bool,
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
            health: Default::default(),
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
            inspector_open: true,
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
    rt: &'a mut UiRuntime,
    st: &'a mut UiState,
    physical: &'a [PhysicalDisplayInfo],
    previews: &'a Previews,
    ndi: &'a NdiStatus,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        egui::RichText::new(tab.title().to_uppercase()).size(10.5).extra_letter_spacing(1.0).into()
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
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
}

fn shortcuts(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
    // Settings opens even while a text field has focus (as on macOS).
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma)) {
        st.settings_open = !st.settings_open;
    }
    if ctx.wants_keyboard_input() || st.pad_learn.is_some() {
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

fn recovery_modal(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
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
                rt.discard_autosave();
                st.recovery = None;
            }
        });
    });
}

/// Non-blocking strip under the transport when movie playback is missing or
/// degraded (see [`livestage_video::capabilities`]).
fn media_banner(ctx: &egui::Context, st: &mut UiState) {
    if st.media_banner_dismissed {
        return;
    }
    let Some(warning) = livestage_video::capabilities::movie_support().warning() else { return };
    egui::TopBottomPanel::top("media-warning")
        .frame(
            egui::Frame::default()
                .fill(crate::theme::wash(crate::theme::standby(), 0.08))
                .stroke(egui::Stroke::new(1.0_f32, crate::theme::wash(crate::theme::standby(), 0.35)))
                .inner_margin(egui::Margin::symmetric(12, 5)),
        )
        .show(ctx, |ui| {
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

/// The inspector: a resizable right-hand panel with a context header (cue
/// type, number, name, state) that collapses to a thin rail.
fn inspector_panel(host: &mut egui::Ui, rt: &mut UiRuntime, st: &mut UiState) {
    use crate::theme;
    use livestage_core::cue::{Cue, CueExt};
    let frame = egui::Frame::default().fill(theme::PANEL_RAISED).stroke(egui::Stroke::new(1.0_f32, theme::BORDER));
    if !st.inspector_open {
        egui::SidePanel::right("inspector-rail").exact_width(30.0).resizable(false).show_separator_line(false).frame(frame.fill(theme::BG_DEEP)).show_inside(host, |ui| {
            let rect = ui.max_rect();
            let r = ui.interact(rect, egui::Id::new("inspector-rail-hit"), egui::Sense::click()).on_hover_text("Show the inspector");
            let p = ui.painter();
            if r.hovered() {
                p.rect_filled(rect, 0.0, theme::CONTROL);
            }
            p.text(egui::pos2(rect.center().x, rect.top() + 16.0), egui::Align2::CENTER_CENTER, "◂", egui::FontId::proportional(13.0), theme::TEXT_DIM);
            let text = egui::epaint::TextShape::new(
                egui::pos2(rect.center().x - 6.0, rect.top() + 120.0),
                ui.fonts(|f| f.layout_no_wrap("I N S P E C T O R".into(), egui::FontId::proportional(10.0), theme::TEXT_DIM)),
                theme::TEXT_DIM,
            )
            .with_angle(-std::f32::consts::FRAC_PI_2);
            p.add(text);
            if r.clicked() {
                st.inspector_open = true;
            }
        });
        return;
    }
    egui::SidePanel::right("inspector")
        .resizable(true)
        .default_width(430.0)
        .width_range(340.0..=760.0)
        .show_separator_line(false)
        .frame(frame.inner_margin(0.0))
        .show_inside(host, |ui| {
            // Header strip.
            let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
            ui.painter().rect_filled(bar, 0.0, theme::BG_DEEP);
            ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, egui::Stroke::new(1.0_f32, theme::BORDER));
            ui.painter().text(bar.left_center() + egui::vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, "I N S P E C T O R", egui::FontId::proportional(10.0), theme::TEXT_DIM);
            let close = egui::Rect::from_center_size(bar.right_center() - egui::vec2(18.0, 0.0), egui::vec2(24.0, 22.0));
            let r = ui.interact(close, egui::Id::new("inspector-collapse"), egui::Sense::click()).on_hover_text("Collapse the inspector");
            if r.hovered() {
                ui.painter().rect_filled(close, 4.0, theme::CONTROL_HOVER);
            }
            ui.painter().text(close.center(), egui::Align2::CENTER_CENTER, "▸", egui::FontId::proportional(13.0), theme::TEXT_DIM);
            if r.clicked() {
                st.inspector_open = false;
            }
            // Context: what is being edited.
            let context = st.selected.and_then(|id| rt.find_cue(id)).map(|c| (c.kind(), c.header().number.clone(), c.header().name.clone(), c.state()));
            egui::Frame::default().inner_margin(egui::Margin { left: 12, right: 12, top: 10, bottom: 8 }).show(ui, |ui| match &context {
                Some((kind, number, name, state)) => {
                    let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
                    let p = ui.painter();
                    let tag = widgets::pill(p, row.left_center(), kind.label(), theme::kind_color(*kind));
                    let (label, c) = match state.color() {
                        Some(_) => (state.label(), theme::state_color(*state).unwrap_or(theme::TEXT_DIM)),
                        None => (state.label(), theme::TEXT_FAINT),
                    };
                    let g = p.layout_no_wrap(label.to_uppercase(), egui::FontId::proportional(10.0), c);
                    widgets::pill(p, egui::pos2(row.right() - g.size().x - 14.0, row.center().y), label, c);
                    let _ = tag;
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(number).font(theme::clock_font(22.0)).color(theme::TEXT));
                        ui.add(egui::Label::new(egui::RichText::new(name).size(18.0).color(theme::TEXT)).truncate());
                    });
                }
                None => {
                    ui.label(egui::RichText::new("No cue selected").size(16.0).color(theme::TEXT_DIM));
                    ui.label(egui::RichText::new("Click a row in the cue grid to inspect and edit it.").color(theme::TEXT_FAINT));
                }
            });
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0_f32, theme::BORDER));
            egui::Frame::default().inner_margin(egui::Margin { left: 12, right: 8, top: 8, bottom: 8 }).show(ui, |ui| {
                if context.is_some() {
                    inspector::show(ui, rt, st);
                }
            });
        });
}

pub fn draw(
    ctx: &egui::Context,
    rt: &mut UiRuntime,
    st: &mut UiState,
    physical: &[PhysicalDisplayInfo],
    previews: &Previews,
    ndi: &NdiStatus,
) {
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
    settings::window(ctx, rt, st);

    // Files dropped on the window are imported into the bundle as cues.
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
    for p in dropped {
        rt.import_and_create_cue(&p);
    }

    use crate::theme;
    egui::TopBottomPanel::top("menu")
        .show_separator_line(false)
        .frame(egui::Frame::default().fill(theme::BG_DEEP).inner_margin(egui::Margin::symmetric(8, 2)))
        .show(ctx, |ui| transport::menu_bar(ui, rt, st));
    egui::TopBottomPanel::top("deck")
        .exact_height(transport::HEIGHT + 20.0)
        .show_separator_line(false)
        .frame(
            egui::Frame::default()
                .fill(theme::BG_DEEP)
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                .inner_margin(egui::Margin { left: 10, right: 10, top: 6, bottom: 12 }),
        )
        .show(ctx, |ui| transport::show(ui, rt, st));
    media_banner(ctx, st);
    egui::TopBottomPanel::bottom("status")
        .exact_height(24.0)
        .show_separator_line(false)
        .frame(
            egui::Frame::default()
                .fill(theme::BG_DEEP)
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                .inner_margin(egui::Margin::symmetric(10, 2)),
        )
        .show(ctx, |ui| transport::status_bar(ui, rt, st));
    transport::import_window(ctx, rt, st);
    // Workspace: tool dock along the bottom (full width), the inspector to
    // the right of the cue grid, the cue grid in the remaining core area.
    egui::CentralPanel::default().frame(egui::Frame::default().fill(theme::BG).inner_margin(6.0)).show(ctx, |ui| {
        let h = ui.available_height();
        egui::TopBottomPanel::bottom("tools")
            .resizable(true)
            .default_height((h * 0.40).max(260.0))
            .height_range(160.0..=(h - 200.0).max(200.0))
            .show_separator_line(false)
            .frame(egui::Frame::default().inner_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 0 }))
            .show_inside(ui, |ui| {
                let style = crate::theme::dock_style(ui.style());
                let mut dock = std::mem::replace(&mut st.dock, DockState::new(vec![]));
                DockArea::new(&mut dock)
                    .id(egui::Id::new("tool-dock"))
                    .style(style)
                    .show_close_buttons(false)
                    .show_leaf_collapse_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_inside(ui, &mut Viewer { rt, st, physical, previews, ndi });
                st.dock = dock;
            });
        inspector_panel(ui, rt, st);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(theme::PANEL_RAISED)
                    .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 4 }),
            )
            .show_inside(ui, |ui| cue_grid::show(ui, rt, st));
    });
}
