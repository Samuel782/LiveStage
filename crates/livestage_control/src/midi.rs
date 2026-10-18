//! MIDI input/output, operator bindings and MIDI Show Control (MSC).

use crate::hub::{ControlEvent, ControlSource};
use crate::ControlError;
use crossbeam_channel::Sender;
use livestage_core::ShowCommand;
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Decoded MIDI message relevant to show control. Channels are 0-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiEvent {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    /// Note-off, including note-on with velocity 0.
    NoteOff { channel: u8, note: u8 },
    ControlChange { channel: u8, controller: u8, value: u8 },
    ProgramChange { channel: u8, program: u8 },
    Msc(MscMessage),
}

/// MIDI Show Control command (ANSI/USITT MSC 1.0, subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MscMessage {
    pub device_id: u8,
    pub command_format: u8,
    pub command: MscCommand,
    pub cue_number: Option<String>,
    pub cue_list: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MscCommand {
    Go,
    Stop,
    Resume,
    TimedGo,
    AllOff,
    Reset,
    Other(u8),
}

/// Parses raw MIDI bytes. `msc_device` filters MSC by device id (0x7F = any).
pub fn parse(bytes: &[u8], msc_device: u8) -> Option<MidiEvent> {
    let status = *bytes.first()?;
    match status & 0xF0 {
        0x90 if bytes.len() >= 3 && bytes[2] > 0 => {
            Some(MidiEvent::NoteOn { channel: status & 0x0F, note: bytes[1], velocity: bytes[2] })
        }
        0x80 | 0x90 if bytes.len() >= 3 => Some(MidiEvent::NoteOff { channel: status & 0x0F, note: bytes[1] }),
        0xB0 if bytes.len() >= 3 => {
            Some(MidiEvent::ControlChange { channel: status & 0x0F, controller: bytes[1], value: bytes[2] })
        }
        0xC0 if bytes.len() >= 2 => Some(MidiEvent::ProgramChange { channel: status & 0x0F, program: bytes[1] }),
        0xF0 if status == 0xF0 => parse_msc(bytes, msc_device).map(MidiEvent::Msc),
        _ => None,
    }
}

fn parse_msc(b: &[u8], filter: u8) -> Option<MscMessage> {
    // F0 7F <device> 02 <format> <command> <data...> F7
    if b.len() < 7 || b[1] != 0x7F || b[3] != 0x02 || *b.last()? != 0xF7 {
        return None;
    }
    let device_id = b[2];
    if filter != 0x7F && device_id != 0x7F && device_id != filter {
        return None;
    }
    let command = match b[5] {
        0x01 => MscCommand::Go,
        0x02 => MscCommand::Stop,
        0x03 => MscCommand::Resume,
        0x04 => MscCommand::TimedGo,
        0x08 => MscCommand::AllOff,
        0x0A => MscCommand::Reset,
        x => MscCommand::Other(x),
    };
    let data = &b[6..b.len() - 1];
    let mut fields = data.split(|&x| x == 0x00).map(|f| String::from_utf8_lossy(f).into_owned());
    let non_empty = |s: Option<String>| s.filter(|s| !s.is_empty());
    let (cue_number, cue_list) = if command == MscCommand::TimedGo {
        // Timed GO carries 5 time bytes before the cue number.
        let rest = data.get(5..).unwrap_or_default();
        let mut f = rest.split(|&x| x == 0).map(|f| String::from_utf8_lossy(f).into_owned());
        (non_empty(f.next()), non_empty(f.next()))
    } else {
        (non_empty(fields.next()), non_empty(fields.next()))
    };
    Some(MscMessage { device_id, command_format: b[4], command, cue_number, cue_list })
}

/// Operator-defined binding of a MIDI trigger to a command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MidiTrigger {
    Note { channel: u8, note: u8 },
    NoteOff { channel: u8, note: u8 },
    ControlChange { channel: u8, controller: u8, threshold: u8 },
    ProgramChange { channel: u8, program: u8 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MidiBinding {
    pub trigger: MidiTrigger,
    pub command: ShowCommand,
}

/// Default bindings: note 60 on ch.1 = GO, 61 = STOP ALL, 62 = PANIC.
pub fn default_bindings() -> Vec<MidiBinding> {
    vec![
        MidiBinding { trigger: MidiTrigger::Note { channel: 0, note: 60 }, command: ShowCommand::Go { list: None } },
        MidiBinding { trigger: MidiTrigger::Note { channel: 0, note: 61 }, command: ShowCommand::StopAll { fade: None } },
        MidiBinding { trigger: MidiTrigger::Note { channel: 0, note: 62 }, command: ShowCommand::Panic { hard: false } },
    ]
}

/// Resolves an event to a command using bindings and MSC semantics.
pub fn to_command(event: &MidiEvent, bindings: &[MidiBinding]) -> Option<ShowCommand> {
    if let MidiEvent::Msc(m) = event {
        return Some(match (m.command, &m.cue_number) {
            (MscCommand::Go | MscCommand::TimedGo, Some(n)) => ShowCommand::GoCue { number: n.clone() },
            (MscCommand::Go | MscCommand::TimedGo, None) => ShowCommand::Go { list: None },
            (MscCommand::Stop, _) => ShowCommand::PauseAll,
            (MscCommand::Resume, _) => ShowCommand::ResumeAll,
            (MscCommand::AllOff, _) => ShowCommand::Panic { hard: false },
            (MscCommand::Reset, _) => ShowCommand::StopAll { fade: None },
            (MscCommand::Other(_), _) => return None,
        });
    }
    bindings.iter().find_map(|b| {
        let hit = match (&b.trigger, event) {
            (MidiTrigger::Note { channel, note }, MidiEvent::NoteOn { channel: c, note: n, .. }) => channel == c && note == n,
            (MidiTrigger::NoteOff { channel, note }, MidiEvent::NoteOff { channel: c, note: n }) => channel == c && note == n,
            (
                MidiTrigger::ControlChange { channel, controller, threshold },
                MidiEvent::ControlChange { channel: c, controller: cc, value },
            ) => channel == c && controller == cc && value >= threshold,
            (MidiTrigger::ProgramChange { channel, program }, MidiEvent::ProgramChange { channel: c, program: p }) => {
                channel == c && program == p
            }
            _ => false,
        };
        hit.then(|| b.command.clone())
    })
}

pub fn list_inputs() -> Vec<String> {
    let Ok(input) = MidiInput::new("LiveStage probe") else { return vec![] };
    input.ports().iter().filter_map(|p| input.port_name(p).ok()).collect()
}

pub fn list_outputs() -> Vec<String> {
    let Ok(out) = MidiOutput::new("LiveStage probe") else { return vec![] };
    out.ports().iter().filter_map(|p| out.port_name(p).ok()).collect()
}

/// An open MIDI input. Dropping it closes the port.
pub struct MidiInputHandler {
    _conn: MidiInputConnection<()>,
    pub port_name: String,
    bindings: Arc<RwLock<Vec<MidiBinding>>>,
    learned: Arc<Mutex<Option<(u8, u8)>>>,
}

impl MidiInputHandler {
    /// Opens the named port (or the first available one).
    pub fn open(
        port_name: Option<&str>,
        bindings: Vec<MidiBinding>,
        msc_device: u8,
        tx: Sender<ControlEvent>,
    ) -> Result<Self, ControlError> {
        let bindings = Arc::new(RwLock::new(bindings));
        let learned = Arc::new(Mutex::new(None));
        let (cb_bindings, cb_learned) = (bindings.clone(), learned.clone());
        let mut input = MidiInput::new("LiveStage").map_err(|e| ControlError::Midi(e.to_string()))?;
        input.ignore(Ignore::TimeAndActiveSense); // keep SysEx for MSC
        let ports = input.ports();
        let port = ports
            .iter()
            .find(|p| port_name.is_none_or(|want| input.port_name(p).is_ok_and(|n| n == want)))
            .ok_or_else(|| ControlError::Midi(format!("no MIDI input {}", port_name.unwrap_or("available"))))?
            .clone();
        let name = input.port_name(&port).unwrap_or_default();
        let source_name = name.clone();
        let conn = input
            .connect(
                &port,
                "livestage-in",
                move |_stamp, bytes, _| {
                    if let Some(ev) = parse(bytes, msc_device) {
                        if let MidiEvent::NoteOn { channel, note, .. } = ev {
                            *cb_learned.lock() = Some((channel, note));
                        }
                        if let Some(command) = to_command(&ev, &cb_bindings.read()) {
                            let _ = tx.send(ControlEvent { source: ControlSource::Midi { port: source_name.clone() }, command });
                        }
                    }
                },
                (),
            )
            .map_err(|e| ControlError::Midi(e.to_string()))?;
        tracing::info!(port = %name, "MIDI input open");
        Ok(Self { _conn: conn, port_name: name, bindings, learned })
    }

    /// Replaces the bindings without reopening the port.
    pub fn set_bindings(&self, bindings: Vec<MidiBinding>) {
        *self.bindings.write() = bindings;
    }

    /// The most recent note-on (channel, note) since the last call — for
    /// "MIDI learn". Call once to clear before learning.
    pub fn take_learned(&self) -> Option<(u8, u8)> {
        self.learned.lock().take()
    }
}

/// MIDI output for control cues.
pub struct MidiOutputHandler {
    conn: MidiOutputConnection,
}

impl MidiOutputHandler {
    pub fn open(port_name: Option<&str>) -> Result<Self, ControlError> {
        let out = MidiOutput::new("LiveStage").map_err(|e| ControlError::Midi(e.to_string()))?;
        let ports = out.ports();
        let port = ports
            .iter()
            .find(|p| port_name.is_none_or(|want| out.port_name(p).is_ok_and(|n| n == want)))
            .ok_or_else(|| ControlError::Midi("no MIDI output".into()))?
            .clone();
        let conn = out.connect(&port, "livestage-out").map_err(|e| ControlError::Midi(e.to_string()))?;
        Ok(Self { conn })
    }
    pub fn send(&mut self, bytes: &[u8]) -> Result<(), ControlError> {
        self.conn.send(bytes).map_err(|e| ControlError::Midi(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_msc_go_with_cue_and_list() {
        // GO cue "12.5" in list "2" for device 1, lighting format.
        let mut b = vec![0xF0, 0x7F, 0x01, 0x02, 0x01, 0x01];
        b.extend_from_slice(b"12.5");
        b.push(0);
        b.extend_from_slice(b"2");
        b.push(0xF7);
        let ev = parse(&b, 0x01).unwrap();
        let MidiEvent::Msc(m) = &ev else { panic!() };
        assert_eq!(m.cue_number.as_deref(), Some("12.5"));
        assert_eq!(m.cue_list.as_deref(), Some("2"));
        assert_eq!(to_command(&ev, &[]), Some(ShowCommand::GoCue { number: "12.5".into() }));
        assert!(parse(&b, 0x05).is_none(), "filtered by device id");
    }

    #[test]
    fn bindings() {
        let b = default_bindings();
        let ev = parse(&[0x90, 60, 100], 0x7F).unwrap();
        assert_eq!(to_command(&ev, &b), Some(ShowCommand::Go { list: None }));
        assert_eq!(parse(&[0x90, 60, 0], 0x7F), Some(MidiEvent::NoteOff { channel: 0, note: 60 }), "note-on velocity 0 is note-off");
        assert_eq!(to_command(&parse(&[0x90, 60, 0], 0x7F).unwrap(), &b), None, "GO is not re-fired on release");
        let pad = vec![MidiBinding {
            trigger: MidiTrigger::NoteOff { channel: 9, note: 36 },
            command: ShowCommand::CartPad { cart: 1, pad: 3, down: false },
        }];
        assert_eq!(to_command(&parse(&[0x89, 36, 64], 0x7F).unwrap(), &pad), Some(ShowCommand::CartPad { cart: 1, pad: 3, down: false }));
    }
}
