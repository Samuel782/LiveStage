//! Settings / Preferences window (⌘, on macOS, Ctrl+, elsewhere).
//!
//! Two kinds of settings, labelled as such: *this Mac* (preferences file)
//! and *this show* (saved in the show bundle so they travel with it).

use super::widgets::toggle;
use super::UiState;
use crate::prefs::Preferences;
use crate::view::UiRuntime;
use crate::theme::{self, Accent, Appearance};
use egui::{ComboBox, DragValue, RichText, Ui};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    General,
    Audio,
    Network,
    Midi,
    Security,
    Timecode,
    Outputs,
    Files,
}

impl Section {
    /// Parses a section name (case-insensitive prefix), for the snapshot harness.
    pub fn from_name(name: &str) -> Section {
        let n = name.to_lowercase();
        Self::ALL.into_iter().find(|s| s.title().to_lowercase().starts_with(&n)).unwrap_or_default()
    }

    const ALL: [Section; 8] = [Section::General, Section::Audio, Section::Network, Section::Midi, Section::Security, Section::Timecode, Section::Outputs, Section::Files];
    fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Audio => "Audio",
            Section::Network => "Network",
            Section::Midi => "MIDI",
            Section::Security => "Security",
            Section::Timecode => "Timecode",
            Section::Outputs => "Outputs & Video",
            Section::Files => "Files & Paths",
        }
    }
}

pub fn window(ctx: &egui::Context, rt: &mut UiRuntime, st: &mut UiState) {
    if !st.settings_open {
        return;
    }
    let before = st.prefs.clone();
    let mut open = true;
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([820.0, 560.0])
        .min_size([620.0, 420.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                sidebar(ui, st);
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_min_width(440.0);
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match st.settings_section {
                        Section::General => general(ui, st),
                        Section::Audio => audio(ui, rt, st),
                        Section::Network => network(ui, rt),
                        Section::Midi => midi(ui, rt, st),
                        Section::Security => security(ui, rt, st),
                        Section::Timecode => timecode(ui, rt, st),
                        Section::Outputs => outputs(ui, st),
                        Section::Files => files(ui, rt, st),
                    });
                });
            });
        });
    if !open {
        st.settings_open = false;
    }
    if st.prefs != before {
        apply_prefs(ctx, &st.prefs);
        rt.office_path = st.prefs.office_path.clone();
        if let Err(e) = st.prefs.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
    }
}

/// Applies the live-adjustable preferences to the UI.
pub fn apply_prefs(ctx: &egui::Context, p: &Preferences) {
    theme::set_accent(p.accent);
    theme::set_appearance(p.appearance);
    theme::apply(ctx);
    ctx.set_zoom_factor(p.ui_scale.clamp(0.7, 2.0));
}

fn sidebar(ui: &mut Ui, st: &mut UiState) {
    ui.vertical(|ui| {
        ui.set_width(150.0);
        for s in Section::ALL {
            let selected = st.settings_section == s;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(150.0, 30.0), egui::Sense::click());
            if selected {
                ui.painter().rect_filled(rect, theme::RADIUS as f32 + 2.0, theme::accent_menu());
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, theme::RADIUS as f32 + 2.0, theme::control_hover());
            }
            let color = if selected { egui::Color32::WHITE } else { theme::text() };
            ui.painter().text(rect.left_center() + egui::vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, s.title(), egui::FontId::proportional(theme::type_scale::BODY + 0.5), color);
            if resp.clicked() {
                st.settings_section = s;
            }
        }
    });
}

fn heading(ui: &mut Ui, title: &str, scope: &str) {
    ui.horizontal(|ui| {
        ui.heading(title);
        ui.label(super::widgets::caption_text(scope));
    });
    ui.add_space(4.0);
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(theme::text_dim()));
}

fn path_field(ui: &mut Ui, id: &str, value: &mut Option<PathBuf>, placeholder: &str) {
    let mut text = value.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
    let r = ui.add(egui::TextEdit::singleline(&mut text).id_salt(id).hint_text(placeholder).desired_width(360.0));
    if r.changed() {
        let t = text.trim().trim_matches(|c| c == '"' || c == '\'').replace("\\ ", " ");
        *value = (!t.is_empty()).then(|| PathBuf::from(t));
    }
    if let Some(p) = value.as_ref().filter(|p| !p.exists()) {
        ui.colored_label(theme::standby(), format!("Not found: {}", p.display()));
    }
}

fn general(ui: &mut Ui, st: &mut UiState) {
    heading(ui, "General", "this Mac");
    egui::Grid::new("set-general").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Appearance");
        ui.horizontal(|ui| {
            for a in Appearance::ALL {
                ui.selectable_value(&mut st.prefs.appearance, a, a.label());
            }
        });
        ui.end_row();
        ui.label("Accent colour");
        ui.horizontal(|ui| {
            for a in Accent::ALL {
                let selected = st.prefs.accent == a;
                let c = a.swatch();
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
                ui.painter().circle_filled(rect.center(), 10.0, c);
                if selected {
                    ui.painter().circle_stroke(rect.center(), 12.5, egui::Stroke::new(2.0_f32, theme::text()));
                }
                if resp.on_hover_text(a.label()).clicked() {
                    st.prefs.accent = a;
                }
            }
        });
        ui.end_row();
        ui.label("Interface size");
        ui.add(egui::Slider::new(&mut st.prefs.ui_scale, 0.8..=1.6).step_by(0.05).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
        ui.end_row();
        ui.label("Quitting");
        toggle(ui, &mut st.prefs.confirm_quit_when_live, "Ask before quitting while outputs are live");
        ui.end_row();
        ui.label("Crash recovery");
        ui.horizontal(|ui| {
            ui.add(DragValue::new(&mut st.prefs.autosave_secs).range(0..=600).suffix(" s"));
            hint(ui, "autosave interval (0 = off)");
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    hint(ui, "State colours (standby amber, active / GO green, stop red) are fixed so they always mean the same thing.");
}

fn audio(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    heading(ui, "Audio", "this show");
    if st.audio_devices.is_empty() {
        st.audio_devices = livestage_audio::list_output_devices();
    }
    let mut changed = false;
    let r = &mut rt.bundle.project.audio;
    egui::Grid::new("set-audio").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Output device");
        ui.horizontal(|ui| {
            let current = r.device_name.clone().unwrap_or_else(|| "System default".into());
            ComboBox::from_id_salt("set-adev").width(300.0).selected_text(current).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut r.device_name, None, "System default").changed();
                for d in &st.audio_devices {
                    let label = format!("{} ({} ch, {} Hz)", d.name, d.max_channels, d.default_sample_rate);
                    changed |= ui.selectable_value(&mut r.device_name, Some(d.name.clone()), label).changed();
                }
            });
            if ui.small_button("Rescan").clicked() {
                st.audio_devices = livestage_audio::list_output_devices();
            }
        });
        ui.end_row();
        ui.label("Sample rate");
        let rate_label = |r: Option<u32>| r.map_or("Device default".to_string(), |r| format!("{r} Hz"));
        ComboBox::from_id_salt("set-rate").selected_text(rate_label(r.sample_rate)).show_ui(ui, |ui| {
            for rate in [None, Some(44_100), Some(48_000), Some(88_200), Some(96_000)] {
                changed |= ui.selectable_value(&mut r.sample_rate, rate, rate_label(rate)).changed();
            }
        });
        ui.end_row();
        ui.label("Buffer size");
        ui.horizontal(|ui| {
            let buf_label = |b: Option<u32>| b.map_or("Device default".to_string(), |b| format!("{b} frames"));
            ComboBox::from_id_salt("set-buf").selected_text(buf_label(r.buffer_frames)).show_ui(ui, |ui| {
                for b in [None, Some(32), Some(64), Some(128), Some(256), Some(512), Some(1024), Some(2048)] {
                    changed |= ui.selectable_value(&mut r.buffer_frames, b, buf_label(b)).changed();
                }
            });
            if let Some(b) = r.buffer_frames {
                let rate = rt.audio.as_ref().map_or(r.sample_rate.unwrap_or(48_000), |a| a.sample_rate()).max(1);
                hint(ui, &format!("≈ {:.1} ms per buffer", b as f64 * 1000.0 / rate as f64));
            }
        });
        ui.end_row();
        ui.label("Show outputs");
        changed |= ui.add(DragValue::new(&mut r.output_channels).range(1..=64).suffix(" channels")).changed();
        ui.end_row();
    });
    hint(ui, "Smaller buffers lower latency but need more headroom; 128–256 frames suit most interfaces.");
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(RichText::new("Apply & restart audio engine").strong()).clicked() {
            rt.start_audio();
            changed = true;
        }
        match (&rt.audio, &rt.audio_error) {
            (Some(a), _) => {
                ui.colored_label(theme::active(), format!("Running: {} · {} Hz · {} ch", a.device_name(), a.sample_rate(), a.channels()));
            }
            (None, Some(e)) => {
                ui.colored_label(theme::stop(), e);
            }
            _ => {}
        }
    });
    hint(ui, "Restarting the engine stops any audio that is playing.");
    if changed {
        rt.mark_changed();
    }
}

/// This computer's IP addresses (what remote devices connect to).
fn local_addresses() -> Vec<(String, std::net::IpAddr)> {
    let mut out = Vec::new();
    #[cfg(unix)]
    {
        // SAFETY: getifaddrs allocates a list that freeifaddrs releases; the
        // entries are only read in between, and sockaddr pointers are cast
        // according to their family.
        unsafe {
            let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
            if libc::getifaddrs(&mut list) == 0 {
                let mut cur = list;
                while !cur.is_null() {
                    let ifa = &*cur;
                    if !ifa.ifa_addr.is_null() {
                        let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                        match (*ifa.ifa_addr).sa_family as i32 {
                            libc::AF_INET => {
                                let a = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                                out.push((name, std::net::IpAddr::V4(std::net::Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr)))));
                            }
                            libc::AF_INET6 => {
                                let a = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                                let ip = std::net::Ipv6Addr::from(a.sin6_addr.s6_addr);
                                // Link-local addresses need a scope id remote apps rarely accept.
                                if (ip.segments()[0] & 0xffc0) != 0xfe80 {
                                    out.push((name, std::net::IpAddr::V6(ip)));
                                }
                            }
                            _ => {}
                        }
                    }
                    cur = ifa.ifa_next;
                }
                libc::freeifaddrs(list);
            }
        }
    }
    #[cfg(not(unix))]
    {
        // The address of the interface that routes outward (no packet is sent).
        if let Some(ip) = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect("192.0.2.1:9").and_then(|_| s.local_addr())).ok().map(|a| a.ip()) {
            out.push(("primary".into(), ip));
        }
    }
    out.retain(|(_, ip)| !ip.is_loopback());
    out.sort_by_key(|(_, ip)| (ip.is_ipv6(), *ip));
    out
}

fn network(ui: &mut Ui, rt: &mut UiRuntime) {
    heading(ui, "Network", "this show");
    let mut changed = false;
    let c = &mut rt.bundle.project.control;
    egui::Grid::new("set-network").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("OSC input");
        ui.horizontal(|ui| {
            changed |= toggle(ui, &mut c.osc_enabled, "enabled").changed();
            changed |= ui.add_enabled(c.osc_enabled, DragValue::new(&mut c.osc_listen_port).range(1024..=65_535).prefix("UDP ")).changed();
        });
        ui.end_row();
        ui.label("WebSocket remote");
        ui.horizontal(|ui| {
            changed |= toggle(ui, &mut c.websocket_enabled, "enabled").changed();
            changed |= ui.add_enabled(c.websocket_enabled, DragValue::new(&mut c.websocket_port).range(1024..=65_535).prefix("TCP ")).changed();
        });
        ui.end_row();
        ui.label("Network access");
        ui.horizontal(|ui| {
            changed |= toggle(ui, &mut c.external_access, "allow other computers").changed();
            hint(ui, if c.external_access { "listening on all interfaces" } else { "this computer only (127.0.0.1)" });
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(RichText::new("Apply & restart control services").strong()).clicked() {
            rt.restart_control_requested = true;
        }
        let osc = rt.osc_port().map_or("OSC off".to_string(), |p| format!("OSC :{p}"));
        let ws = rt.ws_port().map_or("WS off".to_string(), |p| format!("WebSocket :{p}"));
        ui.label(RichText::new(format!("{osc} · {ws}")).color(theme::text_dim()));
    });
    ui.add_space(theme::GRID * 1.5);
    ui.label(RichText::new("This computer's addresses").strong());
    hint(ui, "Point remote devices (tablets, consoles, other show computers) at one of these.");
    let ext = rt.bundle.project.control.external_access;
    egui::Grid::new("set-ifaces").num_columns(3).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
        let addrs = local_addresses();
        if addrs.is_empty() {
            ui.label(RichText::new("No network interfaces with an address").color(theme::text_dim()));
            ui.end_row();
        }
        for (name, ip) in addrs {
            ui.label(RichText::new(name).color(theme::text_dim()));
            ui.label(RichText::new(ip.to_string()).font(theme::clock_font(12.5)));
            match (rt.osc_port(), ext) {
                (Some(p), true) if ip.is_ipv4() => ui.label(RichText::new(format!("osc://{ip}:{p}")).size(theme::type_scale::SMALL).color(theme::text_dim())),
                _ => ui.label(""),
            };
            ui.end_row();
        }
    });
    if !ext {
        hint(ui, "Network access is off: only this computer can reach OSC and the WebSocket.");
    }
    hint(ui, "OSC: /livestage/go, /livestage/cue/<n>/go, /livestage/cue/<n>/seek <secs>, /livestage/slide/next … (see README).");
    if changed {
        rt.mark_changed();
    }
}

fn midi(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    heading(ui, "MIDI", "this show");
    if st.midi_inputs.is_empty() {
        st.midi_inputs = livestage_control::midi::list_inputs();
    }
    let mut changed = false;
    let c = &mut rt.bundle.project.control;
    egui::Grid::new("set-midi-in").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("MIDI input");
        ui.horizontal(|ui| {
            let current = c.midi_input.clone().unwrap_or_else(|| "First available".into());
            ComboBox::from_id_salt("set-midi").width(260.0).selected_text(current).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut c.midi_input, None, "First available").changed();
                for p in &st.midi_inputs {
                    changed |= ui.selectable_value(&mut c.midi_input, Some(p.clone()), p).changed();
                }
            });
            if ui.small_button("Rescan").clicked() {
                st.midi_inputs = livestage_control::midi::list_inputs();
            }
        });
        ui.end_row();
        ui.label("MIDI Show Control");
        ui.horizontal(|ui| {
            let mut all = c.msc_device_id == 0x7F;
            if toggle(ui, &mut all, "all device ids").changed() {
                c.msc_device_id = if all { 0x7F } else { 0 };
                changed = true;
            }
            if !all {
                changed |= ui.add(DragValue::new(&mut c.msc_device_id).range(0..=0x6F).prefix("device ")).changed();
            }
        });
        ui.end_row();
    });
    ui.horizontal(|ui| {
        if ui.button("Apply & restart control services").clicked() {
            rt.restart_control_requested = true;
        }
        ui.label(RichText::new(rt.midi_in.as_ref().map_or("MIDI input closed".to_string(), |m| format!("Listening on {}", m.port_name))).color(theme::text_dim()));
    });
    if changed {
        rt.mark_changed();
    }

    // ---- learn matrix
    ui.add_space(theme::GRID * 1.5);
    ui.label(RichText::new("MIDI learn").strong());
    hint(ui, "Every MIDI trigger in the show. Learn: press it, then play the note on the controller (Esc cancels).");
    use super::cart::PadLearn;
    let learning = st.pad_learn;
    let mut action: Option<Option<PadLearn>> = None;
    let mut clear: Option<(usize, usize)> = None;
    egui::Grid::new("midi-matrix").num_columns(4).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
        for h in ["Trigger", "Does", "Note", ""] {
            ui.label(super::widgets::caption_text(h));
        }
        ui.end_row();
        for b in livestage_control::midi::default_bindings() {
            let note = match b.trigger {
                livestage_control::midi::MidiTrigger::Note { channel, note } => format!("ch {} · note {note}", channel + 1),
                _ => "—".into(),
            };
            ui.label("Built in");
            ui.label(match b.command {
                livestage_control::ShowCommand::Go { .. } => "GO".to_string(),
                livestage_control::ShowCommand::StopAll { .. } => "STOP ALL".to_string(),
                livestage_control::ShowCommand::Panic { .. } => "PANIC".to_string(),
                ref other => format!("{other:?}"),
            });
            ui.label(RichText::new(note).font(theme::clock_font(12.0)));
            ui.label(RichText::new("fixed").size(theme::type_scale::SMALL).color(theme::text_faint()));
            ui.end_row();
        }
        ui.label("MSC");
        ui.label("GO / STOP / RESUME / ALL_OFF");
        ui.label(RichText::new(if rt.bundle.project.control.msc_device_id == 0x7F { "all devices".to_string() } else { format!("device {}", rt.bundle.project.control.msc_device_id) }).font(theme::clock_font(12.0)));
        ui.label("");
        ui.end_row();
        for (ci, cart) in rt.bundle.project.carts.iter().enumerate() {
            for (slot, pad) in cart.pads.iter().enumerate() {
                let Some(pad) = pad else { continue };
                ui.label(format!("{} · pad {}", cart.name, slot + 1));
                ui.label(&livestage_core::cue::Cue::header(&pad.cue).name);
                let me = PadLearn::Midi { cart: ci, slot };
                let listening = learning == Some(me);
                match (listening, pad.midi) {
                    (true, _) => ui.colored_label(theme::standby(), "play a note…"),
                    (false, Some(n)) => ui.label(RichText::new(format!("ch {} · note {}", n.channel + 1, n.note)).font(theme::clock_font(12.0))),
                    (false, None) => ui.label(RichText::new("—").color(theme::text_faint())),
                };
                ui.horizontal(|ui| {
                    if ui.small_button(if listening { "Cancel" } else { "Learn" }).clicked() {
                        action = Some(if listening { None } else { Some(me) });
                    }
                    if pad.midi.is_some() && ui.small_button("Clear").clicked() {
                        clear = Some((ci, slot));
                    }
                });
                ui.end_row();
            }
        }
    });
    if rt.bundle.project.carts.iter().all(|k| k.pads.iter().all(Option::is_none)) {
        hint(ui, "No soundboard pads yet: pads appear here once the Soundboard has some.");
    }
    if let Some(a) = action {
        super::cart::set_learn(rt, st, a);
    }
    if let Some((ci, slot)) = clear {
        if let Some(p) = rt.bundle.project.carts[ci].pad_mut(slot) {
            p.midi = None;
            rt.mark_changed();
        }
    }
    if matches!(st.pad_learn, Some(PadLearn::Midi { .. })) && rt.midi_in.is_none() {
        ui.colored_label(theme::standby(), "No MIDI input is open: choose one above and apply.");
    }
}

fn security(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    heading(ui, "Security", "this show");
    let mut changed = false;
    let c = &mut rt.bundle.project.control;
    let (status, color) = if c.external_access {
        ("Other computers can reach OSC and the WebSocket (WebSocket clients need a token).", theme::standby())
    } else {
        ("Only this computer can reach OSC and the WebSocket.", theme::active())
    };
    ui.colored_label(color, status);
    ui.add_space(theme::GRID);
    egui::Grid::new("set-security").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Network access");
        changed |= toggle(ui, &mut c.external_access, "allow other computers").changed();
        ui.end_row();
        ui.label("OSC allowlist");
        ui.vertical(|ui| {
            let mut text = c.osc_allowlist.join("\n");
            let edit = egui::TextEdit::multiline(&mut text).desired_rows(3).desired_width(280.0).hint_text("192.168.1.20\n10.0.0.0/24");
            if ui.add_enabled(c.external_access, edit).changed() {
                c.osc_allowlist = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
                changed = true;
            }
            let (_, errors) = livestage_control::acl::IpAllowlist::parse(&c.osc_allowlist);
            for e in errors {
                ui.colored_label(theme::stop_text(), e);
            }
            hint(ui, "hosts allowed to send OSC from the network; OSC never runs panic, stop-all, masters, blackout or timecode commands");
        });
        ui.end_row();
    });
    if ui.button("Apply & restart control services").clicked() {
        rt.restart_control_requested = true;
    }
    if changed {
        rt.mark_changed();
    }
    remote_tokens(ui, rt, st);
}

/// Issuing WebSocket session tokens, and revoking them all.
fn remote_tokens(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    use livestage_control::auth::RemoteAuth;
    ui.add_space(10.0);
    ui.label(RichText::new("Remote access tokens").strong());
    hint(ui, "WebSocket clients must present a token: `Authorization: Bearer <token>`, or `ws://host:port/?token=<token>` from a browser.");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut st.token_label).desired_width(140.0).hint_text("client name"));
        ui.add(DragValue::new(&mut st.token_days).range(1..=365).suffix(" days"));
        if ui.button("Issue token").clicked() {
            let issued = crate::runtime::remote_key_path()
                .ok_or_else(|| "no configuration folder".to_string())
                .and_then(|p| RemoteAuth::load_or_create(&p).map_err(|e| e.to_string()))
                .map(|auth| auth.issue(&st.token_label, std::time::Duration::from_secs(st.token_days as u64 * 86_400), std::time::SystemTime::now()));
            match issued {
                Ok(t) => {
                    rt.log(crate::runtime::LogLevel::Info, format!("remote token issued for `{}` ({} days)", st.token_label, st.token_days));
                    st.issued_token = Some(t);
                }
                Err(e) => rt.log(crate::runtime::LogLevel::Error, format!("remote token: {e}")),
            }
        }
        if ui.button("Revoke all tokens").on_hover_text("Replaces the key: every token issued so far stops working").clicked() {
            rt.rotate_remote_key();
            st.issued_token = None;
        }
    });
    if let Some(token) = &st.issued_token {
        ui.horizontal(|ui| {
            let mut shown = token.clone();
            ui.add(egui::TextEdit::singleline(&mut shown).desired_width(420.0).font(egui::TextStyle::Monospace));
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(token.clone());
            }
        });
        hint(ui, "Copy it to the client now. Anyone holding it can run every command, panic included, until it expires or tokens are revoked.");
    }
}

fn timecode(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    use livestage_control::{ControlSource, ShowCommand};
    use livestage_core::timecode::{ClockBase, FrameRate, ListTimecode, LockState, OnTimecodeStop, TimecodeSource};
    heading(ui, "Timecode", "this show");
    if st.midi_inputs.is_empty() {
        st.midi_inputs = livestage_control::midi::list_inputs();
    }

    // Live readout
    let t = rt.timecode.transport();
    let (color, state) = match t.state {
        LockState::Locked => (theme::active(), if t.running { "Rolling" } else { "Locked" }),
        LockState::Freewheel => (theme::standby(), "Freewheel"),
        LockState::Locking => (theme::standby(), "Locking…"),
        LockState::Stopped => (theme::text_dim(), "Stopped"),
    };
    egui::Frame::new().fill(theme::raised()).corner_radius(8).inner_margin(10).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(t.timecode().to_string()).font(theme::clock_font(34.0)).color(if t.running { theme::active() } else { theme::text() }));
            ui.vertical(|ui| {
                ui.colored_label(color, RichText::new(state).strong());
                ui.label(RichText::new(format!("{} · {}", t.rate, rt.timecode_input)).size(12.0).color(theme::text_dim()));
                if rt.timecode.config().source != TimecodeSource::Internal && t.state != LockState::Stopped {
                    ui.label(
                        RichText::new(format!("drift {:+.0} ppm · jitter {:+.1} ms", t.drift_ppm(), rt.timecode.lock_error_us() / 1000.0))
                            .size(12.0)
                            .color(theme::text_dim()),
                    );
                }
            });
        });
        if rt.timecode.config().source == TimecodeSource::Internal {
            ui.horizontal(|ui| {
                if ui.button(if t.running { "Stop" } else { "Start" }).clicked() {
                    rt.command(ControlSource::Ui, if t.running { ShowCommand::TimecodeStop } else { ShowCommand::TimecodeStart });
                }
                if ui.button("Reset").on_hover_text("Stop and park at the start timecode").clicked() {
                    rt.timecode.reset();
                }
                ui.separator();
                ui.label("Locate");
                // Shows the live position; typing a label and pressing Enter locates.
                let mut target = t.timecode();
                if super::widgets::timecode_field(ui, "set-tc-locate", &mut target, t.rate) {
                    rt.command(ControlSource::Ui, ShowCommand::TimecodeLocate { timecode: target });
                }
            });
        }
    });
    ui.add_space(8.0);

    let mut changed = false;
    let mut cfg = rt.bundle.project.timecode.clone();
    egui::Grid::new("set-tc").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Source");
        ComboBox::from_id_salt("set-tc-src").width(220.0).selected_text(cfg.source.label()).show_ui(ui, |ui| {
            for src in TimecodeSource::ALL {
                changed |= ui.selectable_value(&mut cfg.source, src, src.label()).changed();
            }
        });
        ui.end_row();
        match cfg.source {
            TimecodeSource::Internal => {
                ui.label("Frame rate");
                ComboBox::from_id_salt("set-tc-rate").selected_text(cfg.rate.label()).show_ui(ui, |ui| {
                    for r in FrameRate::ALL {
                        changed |= ui.selectable_value(&mut cfg.rate, r, r.label()).changed();
                    }
                });
                ui.end_row();
                ui.label("Clock");
                ComboBox::from_id_salt("set-tc-base").selected_text(cfg.internal_base.label()).show_ui(ui, |ui| {
                    for b in [ClockBase::Audio, ClockBase::Wall] {
                        changed |= ui.selectable_value(&mut cfg.internal_base, b, b.label()).changed();
                    }
                });
                ui.end_row();
                ui.label("Start at");
                changed |= super::widgets::timecode_field(ui, "set-tc-start", &mut cfg.internal_start, cfg.rate);
                ui.end_row();
            }
            TimecodeSource::Mtc => {
                ui.label("MIDI input");
                ui.horizontal(|ui| {
                    let current = cfg.mtc_input.clone().unwrap_or_else(|| "First available".into());
                    ComboBox::from_id_salt("set-tc-midi").width(260.0).selected_text(current).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut cfg.mtc_input, None, "First available").changed();
                        for p in &st.midi_inputs {
                            changed |= ui.selectable_value(&mut cfg.mtc_input, Some(p.clone()), p).changed();
                        }
                    });
                    if ui.small_button("Rescan").clicked() {
                        st.midi_inputs = livestage_control::midi::list_inputs();
                    }
                });
                ui.end_row();
            }
            TimecodeSource::ArtNet => {
                ui.label("Art-Net port");
                changed |= ui.add(DragValue::new(&mut cfg.artnet_port).range(1024..=65_535).prefix("UDP ")).changed();
                ui.end_row();
            }
            TimecodeSource::Ltc => {
                if st.audio_inputs.is_empty() {
                    st.audio_inputs = livestage_audio::ltc::list_input_devices();
                }
                ui.label("Audio input");
                ui.horizontal(|ui| {
                    let current = cfg.ltc_input.clone().unwrap_or_else(|| "System default".into());
                    ComboBox::from_id_salt("set-tc-ltc-in").width(260.0).selected_text(current).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut cfg.ltc_input, None, "System default").changed();
                        for (name, ch) in &st.audio_inputs {
                            changed |= ui.selectable_value(&mut cfg.ltc_input, Some(name.clone()), format!("{name} ({ch} ch)")).changed();
                        }
                    });
                    if ui.small_button("Rescan").clicked() {
                        st.audio_inputs = livestage_audio::ltc::list_input_devices();
                    }
                });
                ui.end_row();
                ui.label("Channel");
                ui.horizontal(|ui| {
                    changed |= ui.add(DragValue::new(&mut cfg.ltc_channel).range(1..=64)).changed();
                    hint(ui, "24, 25, 29.97 (DF) and 30 fps are detected from the signal");
                });
                ui.end_row();
            }
        }
        if cfg.source != TimecodeSource::Internal {
            ui.label("Freewheel");
            ui.horizontal(|ui| {
                let mut ms = cfg.lock.freewheel.as_micros() / 1000;
                if ui.add(DragValue::new(&mut ms).range(0..=5_000).suffix(" ms")).changed() {
                    cfg.lock.freewheel = livestage_core::ShowTime::from_millis(ms);
                    changed = true;
                }
                hint(ui, "keep rolling through dropouts this long");
            });
            ui.end_row();
            ui.label("Jump threshold");
            ui.horizontal(|ui| {
                changed |= ui.add(DragValue::new(&mut cfg.lock.jump_frames).range(1..=100).suffix(" frames")).changed();
                hint(ui, "larger errors re-locate instead of drifting back");
            });
            ui.end_row();
        }
    });

    ui.add_space(10.0);
    ui.label(RichText::new("Timecode out").strong());
    hint(ui, "Sends the show timecode (the internal generator, or the source it follows) so other machines can chase LiveStage.");
    if st.midi_outputs.is_empty() {
        st.midi_outputs = livestage_control::midi::list_outputs();
    }
    egui::Grid::new("set-tc-out").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        let out = &mut cfg.output;
        changed |= toggle(ui, &mut out.mtc, "MTC").changed();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(out.mtc, |ui| {
                let current = out.mtc_port.clone().unwrap_or_else(|| "First available".into());
                ComboBox::from_id_salt("set-tc-mtc-out").width(260.0).selected_text(current).show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut out.mtc_port, None, "First available").changed();
                    for p in &st.midi_outputs {
                        changed |= ui.selectable_value(&mut out.mtc_port, Some(p.clone()), p).changed();
                    }
                });
            });
            if ui.small_button("Rescan").clicked() {
                st.midi_outputs = livestage_control::midi::list_outputs();
            }
        });
        ui.end_row();
        changed |= toggle(ui, &mut out.ltc, "LTC").changed();
        ui.add_enabled_ui(out.ltc, |ui| {
            ui.horizontal(|ui| {
                changed |= ui.add(DragValue::new(&mut out.ltc_channel).range(1..=64).prefix("output ")).changed();
                changed |= ui.add(DragValue::new(&mut out.ltc_level_db).range(-30.0..=0.0).speed(0.25).suffix(" dBFS")).changed();
                hint(ui, "on the show's audio device, after the master: panics do not mute it");
            });
        });
        ui.end_row();
        changed |= toggle(ui, &mut out.artnet, "Art-Net").changed();
        ui.add_enabled_ui(out.artnet, |ui| {
            ui.horizontal(|ui| {
                let draft = st.artnet_tc_dest.get_or_insert_with(|| out.artnet_destination.clone());
                let r = ui.add(egui::TextEdit::singleline(draft).desired_width(180.0).hint_text("255.255.255.255"));
                if r.lost_focus() {
                    let typed = draft.trim().to_string();
                    let typed = if typed.is_empty() { "255.255.255.255".to_string() } else { typed };
                    if typed != out.artnet_destination {
                        out.artnet_destination = typed;
                        changed = true;
                    }
                    st.artnet_tc_dest = None;
                } else if !r.has_focus() {
                    st.artnet_tc_dest = None;
                }
                hint(ui, "OpTimeCode to a broadcast or node address; paused while following Art-Net timecode");
            });
        });
        ui.end_row();
    });
    if !rt.timecode_output.is_empty() && rt.timecode_output != "off" {
        hint(ui, &format!("Sending {}", rt.timecode_output));
    }
    if changed {
        rt.bundle.project.timecode = cfg;
        rt.apply_timecode_config();
        rt.mark_changed();
    }

    ui.add_space(10.0);
    ui.label(RichText::new("Cue lists in timecode mode").strong());
    hint(ui, "Cues with a timecode trigger (Inspector → Timecode) fire on their own; chasing, pausing and re-sync follow the stream.");
    let mut list_changed = false;
    for list in &mut rt.bundle.project.cue_lists {
        ui.horizontal(|ui| {
            let mut on = list.timecode.is_some();
            if toggle(ui, &mut on, &list.name).changed() {
                list.timecode = on.then(ListTimecode::new);
                list_changed = true;
            }
            if let Some(tc) = &mut list.timecode {
                ComboBox::from_id_salt(("set-tc-list", list.id)).selected_text(format!("on stop: {}", tc.on_stop.label())).show_ui(ui, |ui| {
                    for o in OnTimecodeStop::ALL {
                        list_changed |= ui.selectable_value(&mut tc.on_stop, o, o.label()).changed();
                    }
                });
                let triggers = list.cues.iter().filter(|c| livestage_core::cue::Cue::header(*c).timecode.is_some()).count();
                hint(ui, &format!("{triggers} triggered cue(s)"));
            }
        });
    }
    if list_changed {
        rt.mark_changed();
    }
    ui.add_space(6.0);
    hint(ui, "Timeline cues become slaves in the Inspector (Timecode slave). OSC: /livestage/timecode/start | stop | locate \"HH:MM:SS:FF\".");
}

fn outputs(ui: &mut Ui, st: &mut UiState) {
    heading(ui, "Outputs & Video", "this Mac");
    let p = &mut st.prefs;
    egui::Grid::new("set-outputs").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Program outputs");
        ui.vertical(|ui| {
            toggle(ui, &mut p.outputs_on_launch, "Open outputs when a show is loaded");
            toggle(ui, &mut p.outputs_always_on_top, "Keep kiosk outputs above other windows");
            toggle(ui, &mut p.hide_cursor_on_outputs, "Hide the mouse pointer on kiosk outputs");
        });
        ui.end_row();
        ui.label("Movie playback");
        ui.vertical(|ui| {
            use livestage_video::capabilities::{movie_support, MovieSupport};
            let media = movie_support();
            match media.warning() {
                Some(w) => {
                    ui.colored_label(theme::standby(), w);
                }
                None => {
                    ui.label(media.summary());
                }
            }
            if let MovieSupport::Ready { missing_optional, .. } = media {
                if !missing_optional.is_empty() {
                    hint(ui, &format!("Not in this FFmpeg build: {}", missing_optional.join(", ")));
                }
            }
            if !media.movies_available() {
                hint(ui, "Release builds include FFmpeg (scripts/build-release.sh); see README → Building.");
            }
        });
        ui.end_row();
        ui.label("Video decoding");
        ui.vertical(|ui| {
            toggle(ui, &mut p.hardware_decode, "Hardware decoding (VideoToolbox / D3D11VA / VAAPI)");
            hint(ui, "Takes effect after restarting LiveStage.");
        });
        ui.end_row();
        ui.label("NDI runtime folder");
        ui.vertical(|ui| {
            path_field(ui, "set-ndi", &mut p.ndi_runtime_dir, "SDK default locations");
            let found = livestage_video::ndi::detect_runtime();
            match found {
                Some(f) => hint(ui, &format!("In use: {}", f.display())),
                None => {
                    ui.colored_label(theme::standby(), "NDI runtime not found — install NDI Tools / the NDI runtime.");
                }
            }
            hint(ui, "Takes effect after restarting LiveStage.");
        });
        ui.end_row();
    });
    hint(ui, "Window changes apply the next time outputs are opened.");
}

fn files(ui: &mut Ui, rt: &mut UiRuntime, st: &mut UiState) {
    heading(ui, "Files & Paths", "this Mac");
    let root = rt.bundle.root().to_path_buf();
    let p = &mut st.prefs;
    egui::Grid::new("set-files").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
        ui.label("Show at startup");
        ui.vertical(|ui| {
            path_field(ui, "set-startup", &mut p.startup_show, "Demo show");
            if ui.small_button("Use the current show").clicked() {
                p.startup_show = Some(root.clone());
            }
        });
        ui.end_row();
        ui.label("Media folder");
        ui.vertical(|ui| {
            path_field(ui, "set-media", &mut p.media_folder, "pre-fills the import dialog");
        });
        ui.end_row();
        ui.label("LibreOffice");
        ui.vertical(|ui| {
            path_field(ui, "set-office", &mut p.office_path, "auto-detect soffice");
            match p.office_path.clone().filter(|x| x.is_file()).or_else(livestage_video::slides::find_office) {
                Some(o) => hint(ui, &format!("PowerPoint / Keynote import via {}", o.display())),
                None => hint(ui, "Not installed: .pptx uses the built-in renderer; .ppt / .key / .odp need LibreOffice."),
            }
        });
        ui.end_row();
        ui.label("Current show");
        ui.label(RichText::new(root.display().to_string()).color(theme::text_dim()));
        ui.end_row();
        ui.label("Preferences file");
        ui.label(RichText::new(Preferences::path().map_or("—".into(), |p| p.display().to_string())).color(theme::text_dim()));
        ui.end_row();
    });
}
