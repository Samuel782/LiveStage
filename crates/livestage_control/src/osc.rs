//! OSC server (UDP) and sender.
//!
//! Address space (QLab-style aliases accepted without the prefix):
//!
//! | Address                          | Args            | Command          |
//! |----------------------------------|-----------------|------------------|
//! | `/livestage/go`                  |                 | GO               |
//! | `/livestage/go/{number}`         |                 | GO cue           |
//! | `/livestage/cue/{number}/go`     |                 | GO cue           |
//! | `/livestage/stop`                | `[fade secs]`   | Stop all         |
//! | `/livestage/panic`               | `[1 = hard]`    | Panic            |
//! | `/livestage/pause` / `resume`    |                 | Pause / resume   |
//! | `/livestage/standby/next|previous` |               | Move playhead    |
//! | `/livestage/standby/{number}`    |                 | Set standby      |
//! | `/livestage/master`              | `dB`            | Master gain      |
//! | `/livestage/status`              |                 | Status request   |
//! | `/livestage/timecode/start|stop` |                 | Internal TC transport |
//! | `/livestage/timecode/locate`     | `"HH:MM:SS:FF"` | Internal TC locate |

use crate::hub::{ControlEvent, ControlSource};
use crate::ControlError;
use crossbeam_channel::Sender;
use livestage_core::action::OscArg;
use livestage_core::{ShowCommand, ShowTime};
use rosc::{OscMessage, OscPacket, OscType};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

fn arg_f32(args: &[OscType], i: usize) -> Option<f32> {
    match args.get(i)? {
        OscType::Float(f) => Some(*f),
        OscType::Double(d) => Some(*d as f32),
        OscType::Int(n) => Some(*n as f32),
        OscType::Long(n) => Some(*n as f32),
        OscType::String(s) => s.parse().ok(),
        OscType::Bool(b) => Some(*b as u8 as f32),
        _ => None,
    }
}

/// Maps one OSC message to a show command.
pub fn parse_message(msg: &OscMessage) -> Option<ShowCommand> {
    let addr = msg.addr.trim_end_matches('/');
    let addr = addr.strip_prefix("/livestage").unwrap_or(addr);
    let parts: Vec<&str> = addr.split('/').filter(|p| !p.is_empty()).collect();
    let cmd = match parts.as_slice() {
        ["go"] => ShowCommand::Go { list: None },
        ["go", number] | ["cue", number, "go"] => ShowCommand::GoCue { number: number.to_string() },
        ["stop"] | ["stop", "all"] => {
            ShowCommand::StopAll { fade: arg_f32(&msg.args, 0).map(|s| ShowTime::from_secs_f64(s as f64)) }
        }
        ["panic"] => ShowCommand::Panic { hard: arg_f32(&msg.args, 0).is_some_and(|v| v >= 1.0) },
        ["pause"] => ShowCommand::PauseAll,
        ["resume"] => ShowCommand::ResumeAll,
        ["standby", "next"] | ["playhead", "next"] => ShowCommand::StandbyNext,
        ["standby", "previous"] | ["playhead", "previous"] => ShowCommand::StandbyPrevious,
        ["standby", number] | ["playhead", number] => ShowCommand::SetStandby { number: number.to_string() },
        ["master"] => ShowCommand::SetMasterGain { db: arg_f32(&msg.args, 0)? },
        ["status"] => ShowCommand::RequestStatus,
        ["cue", number, "seek"] | ["seek", number] => ShowCommand::SeekCue {
            number: number.to_string(),
            position: ShowTime::from_secs_f64(arg_f32(&msg.args, 0)?.max(0.0) as f64),
        },
        ["outputs"] => ShowCommand::SetOutputsFullscreen { enabled: arg_f32(&msg.args, 0).is_none_or(|v| v >= 1.0) },
        ["slide", "next"] => ShowCommand::NextSlide,
        ["slide", "previous"] | ["slide", "prev"] => ShowCommand::PrevSlide,
        ["slide"] => ShowCommand::GotoSlide { slide: arg_f32(&msg.args, 0)?.max(1.0) as usize },
        ["timecode", "start"] => ShowCommand::TimecodeStart,
        ["timecode", "stop"] => ShowCommand::TimecodeStop,
        ["timecode", "locate"] => match msg.args.first()? {
            OscType::String(s) => ShowCommand::TimecodeLocate { timecode: s.parse().ok()? },
            _ => return None,
        },
        _ => return None,
    };
    Some(cmd)
}

fn walk(packet: OscPacket, out: &mut Vec<ShowCommand>) {
    match packet {
        OscPacket::Message(m) => match parse_message(&m) {
            Some(c) => out.push(c),
            None => tracing::debug!(addr = %m.addr, "unhandled OSC address"),
        },
        OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| walk(p, out)),
    }
}

pub struct OscServer {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub port: u16,
}

impl OscServer {
    pub fn start(port: u16, tx: Sender<ControlEvent>) -> Result<Self, ControlError> {
        let socket = UdpSocket::bind(("0.0.0.0", port))?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let port = socket.local_addr()?.port();
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let handle = std::thread::Builder::new().name("livestage-osc".into()).spawn(move || {
            let mut buf = [0u8; rosc::decoder::MTU];
            while r.load(Ordering::Relaxed) {
                let Ok((n, peer)) = socket.recv_from(&mut buf) else { continue };
                match rosc::decoder::decode_udp(&buf[..n]) {
                    Ok((_, packet)) => {
                        let mut cmds = Vec::new();
                        walk(packet, &mut cmds);
                        for command in cmds {
                            let _ = tx.send(ControlEvent { source: ControlSource::Osc { peer: peer.to_string() }, command });
                        }
                    }
                    Err(e) => tracing::debug!(%peer, "bad OSC packet: {e:?}"),
                }
            }
        })?;
        tracing::info!(port, "OSC server listening");
        Ok(Self { running, handle: Some(handle), port })
    }
}

impl Drop for OscServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Fire-and-forget OSC sender for control cues.
pub struct OscSender {
    socket: UdpSocket,
}

impl OscSender {
    pub fn new() -> Result<Self, ControlError> {
        Ok(Self { socket: UdpSocket::bind("0.0.0.0:0")? })
    }

    pub fn send(&self, host: &str, port: u16, address: &str, args: &[OscArg]) -> Result<(), ControlError> {
        let args = args
            .iter()
            .map(|a| match a {
                OscArg::Int(i) => OscType::Int(*i),
                OscArg::Float(f) => OscType::Float(*f),
                OscArg::String(s) => OscType::String(s.clone()),
                OscArg::Bool(b) => OscType::Bool(*b),
            })
            .collect();
        let packet = OscPacket::Message(OscMessage { addr: address.to_string(), args });
        let bytes = rosc::encoder::encode(&packet).map_err(|e| ControlError::Osc(format!("{e:?}")))?;
        self.socket.send_to(&bytes, (host, port))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(addr: &str, args: Vec<OscType>) -> OscMessage {
        OscMessage { addr: addr.into(), args }
    }

    #[test]
    fn address_space() {
        assert_eq!(parse_message(&msg("/livestage/go", vec![])), Some(ShowCommand::Go { list: None }));
        assert_eq!(parse_message(&msg("/go", vec![])), Some(ShowCommand::Go { list: None }));
        assert_eq!(
            parse_message(&msg("/livestage/cue/12.5/go", vec![])),
            Some(ShowCommand::GoCue { number: "12.5".into() })
        );
        assert_eq!(parse_message(&msg("/livestage/panic", vec![OscType::Int(1)])), Some(ShowCommand::Panic { hard: true }));
        assert_eq!(
            parse_message(&msg("/livestage/cue/3/seek", vec![OscType::Float(12.5)])),
            Some(ShowCommand::SeekCue { number: "3".into(), position: ShowTime::from_millis(12_500) })
        );
        assert_eq!(parse_message(&msg("/livestage/slide/next", vec![])), Some(ShowCommand::NextSlide));
        assert_eq!(parse_message(&msg("/livestage/slide", vec![OscType::Int(4)])), Some(ShowCommand::GotoSlide { slide: 4 }));
        assert_eq!(
            parse_message(&msg("/livestage/stop", vec![OscType::Float(2.0)])),
            Some(ShowCommand::StopAll { fade: Some(ShowTime::from_secs(2)) })
        );
        assert_eq!(parse_message(&msg("/livestage/timecode/start", vec![])), Some(ShowCommand::TimecodeStart));
        assert_eq!(
            parse_message(&msg("/livestage/timecode/locate", vec![OscType::String("01:00:00:00".into())])),
            Some(ShowCommand::TimecodeLocate { timecode: "01:00:00:00".parse().unwrap() })
        );
        assert_eq!(parse_message(&msg("/livestage/timecode/locate", vec![OscType::String("nope".into())])), None);
        assert_eq!(parse_message(&msg("/nope", vec![])), None);
    }

    #[test]
    fn udp_roundtrip() {
        let hub = crate::ControlHub::default();
        let server = OscServer::start(0, hub.sender()).unwrap();
        let sender = OscSender::new().unwrap();
        sender.send("127.0.0.1", server.port, "/livestage/go/7", &[]).unwrap();
        let ev = (0..50)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(10));
                hub.drain().next()
            })
            .expect("OSC event");
        assert_eq!(ev.command, ShowCommand::GoCue { number: "7".into() });
    }
}
