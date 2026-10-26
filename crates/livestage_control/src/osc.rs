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
//! | `/livestage/cart/{c}/pad/{p}`    | `[1 / 0]`       | Press / release a cart pad |
//! | `/livestage/cart/stop`           |                 | Stop all pads    |
//! | `/livestage/sub/{n}`             | `level 0..1`    | Submaster fader  |
//! | `/livestage/sub/{n}/flash`       | `[1 / 0]`       | Submaster flash  |
//! | `/livestage/gm`                  | `level 0..1`    | Grand master     |
//! | `/livestage/blackout`            | `[1 / 0]`       | Desk blackout    |
//! | `/livestage/fx/{n}`              | `[1 / 0]`       | Start / stop effect |
//! | `/livestage/programmer/clear`    |                 | Clear programmer |
//! | `/livestage/timecode/start|stop` |                 | Internal TC transport |
//! | `/livestage/timecode/locate`     | `"HH:MM:SS:FF"` | Internal TC locate |
//!
//! ## Security
//!
//! OSC is unauthenticated UDP, so the server:
//!
//! - listens on 127.0.0.1 unless external access is enabled;
//! - accepts packets only from this computer and the hosts on the
//!   [`IpAllowlist`];
//! - never runs [sensitive](ShowCommand::is_sensitive) commands (panic, stop
//!   all, masters, blackout, submasters, output and timecode control): those
//!   need the authenticated WebSocket API.

use crate::acl::IpAllowlist;
use crate::hub::{ControlEvent, ControlSource};
use crate::ControlError;
use crossbeam_channel::Sender;
use livestage_core::action::OscArg;
use livestage_core::{ShowCommand, ShowTime};
use rosc::{OscMessage, OscPacket, OscType};
use std::collections::HashSet;
use std::net::{IpAddr, UdpSocket};
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
        ["cart", "stop"] => ShowCommand::CartStopAll,
        ["cart", cart, "pad", pad] => ShowCommand::CartPad {
            cart: cart.parse().ok()?,
            pad: pad.parse().ok()?,
            // No argument = press; 0 releases (for hold pads).
            down: arg_f32(&msg.args, 0).is_none_or(|v| v >= 0.5),
        },
        ["sub", n] => ShowCommand::SetSubmaster { sub: n.parse().ok()?, level: arg_f32(&msg.args, 0)?.clamp(0.0, 1.0) },
        ["sub", n, "flash"] => ShowCommand::FlashSubmaster { sub: n.parse().ok()?, down: arg_f32(&msg.args, 0).is_none_or(|v| v >= 0.5) },
        ["gm"] | ["grandmaster"] => ShowCommand::SetGrandMaster { level: arg_f32(&msg.args, 0)?.clamp(0.0, 1.0) },
        ["blackout"] | ["dbo"] => ShowCommand::SetBlackout { enabled: arg_f32(&msg.args, 0).is_none_or(|v| v >= 0.5) },
        ["fx", n] => ShowCommand::SetEffect { effect: n.parse().ok()?, running: arg_f32(&msg.args, 0).is_none_or(|v| v >= 0.5) },
        ["programmer", "clear"] => ShowCommand::ClearProgrammer,
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

pub(crate) fn walk(packet: OscPacket, out: &mut Vec<ShowCommand>) {
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
    /// Listens on `bind:port` (127.0.0.1 unless external access is on),
    /// taking packets from `allow`ed hosts only.
    pub fn start(bind: IpAddr, port: u16, tx: Sender<ControlEvent>, allow: IpAllowlist) -> Result<Self, ControlError> {
        let socket = UdpSocket::bind((bind, port))?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let port = socket.local_addr()?.port();
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let handle = std::thread::Builder::new().name("livestage-osc".into()).spawn(move || {
            let mut buf = [0u8; rosc::decoder::MTU];
            let mut reported = Reported::default();
            while r.load(Ordering::Relaxed) {
                let Ok((n, peer)) = socket.recv_from(&mut buf) else { continue };
                if !allow.allows(peer.ip()) {
                    if reported.first(peer.ip(), "denied") {
                        tracing::warn!(%peer, "OSC from a host not on the allowlist ignored");
                    }
                    continue;
                }
                // Packets come from the network: one that trips a decoder bug
                // is dropped, the server keeps listening.
                let decoded = std::panic::catch_unwind(|| {
                    rosc::decoder::decode_udp(&buf[..n]).map(|(_, packet)| {
                        let mut cmds = Vec::new();
                        walk(packet, &mut cmds);
                        cmds
                    })
                });
                match decoded {
                    Ok(Ok(cmds)) => {
                        for command in cmds {
                            if command.is_sensitive() {
                                if reported.first(peer.ip(), "sensitive") {
                                    tracing::warn!(%peer, ?command, "sensitive command refused over OSC; use the authenticated WebSocket API");
                                }
                                continue;
                            }
                            let _ = tx.send(ControlEvent { source: ControlSource::Osc { peer: peer.to_string() }, command });
                        }
                    }
                    Ok(Err(e)) => tracing::debug!(%peer, "bad OSC packet: {e:?}"),
                    Err(_) => tracing::warn!(%peer, "OSC packet crashed the decoder; dropped"),
                }
            }
        })?;
        tracing::info!(%bind, port, "OSC server listening");
        Ok(Self { running, handle: Some(handle), port })
    }
}

/// Logs a refusal once per host and kind (bounded, so a flood of spoofed
/// sources cannot grow it without limit).
#[derive(Default)]
struct Reported(HashSet<(IpAddr, &'static str)>);

impl Reported {
    fn first(&mut self, ip: IpAddr, kind: &'static str) -> bool {
        if self.0.len() >= 256 {
            return false;
        }
        self.0.insert((ip, kind))
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
        assert_eq!(
            parse_message(&msg("/livestage/cart/2/pad/16", vec![])),
            Some(ShowCommand::CartPad { cart: 2, pad: 16, down: true })
        );
        assert_eq!(
            parse_message(&msg("/livestage/cart/1/pad/3", vec![OscType::Int(0)])),
            Some(ShowCommand::CartPad { cart: 1, pad: 3, down: false })
        );
        assert_eq!(parse_message(&msg("/livestage/cart/x/pad/3", vec![])), None);
        assert_eq!(
            parse_message(&msg("/livestage/sub/2", vec![OscType::Float(0.75)])),
            Some(ShowCommand::SetSubmaster { sub: 2, level: 0.75 })
        );
        assert_eq!(parse_message(&msg("/livestage/gm", vec![OscType::Float(1.5)])), Some(ShowCommand::SetGrandMaster { level: 1.0 }));
        assert_eq!(parse_message(&msg("/livestage/fx/3", vec![OscType::Int(0)])), Some(ShowCommand::SetEffect { effect: 3, running: false }));
        assert_eq!(parse_message(&msg("/livestage/blackout", vec![])), Some(ShowCommand::SetBlackout { enabled: true }));
        assert_eq!(parse_message(&msg("/nope", vec![])), None);
    }

    #[test]
    fn udp_roundtrip() {
        let hub = crate::ControlHub::default();
        let server = OscServer::start(IpAddr::from([127, 0, 0, 1]), 0, hub.sender(), IpAllowlist::default()).unwrap();
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

    /// A real socket: from this computer GO gets through, a panic does not.
    #[test]
    fn server_refuses_sensitive_commands() {
        let hub = crate::ControlHub::default();
        let server = OscServer::start(IpAddr::from([127, 0, 0, 1]), 0, hub.sender(), IpAllowlist::default()).unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let send = |addr: &str, args: Vec<OscType>| {
            let msg = rosc::encoder::encode(&OscPacket::Message(OscMessage { addr: addr.into(), args })).unwrap();
            tx.send_to(&msg, ("127.0.0.1", server.port)).unwrap();
        };
        send("/livestage/panic", vec![OscType::Int(1)]);
        send("/livestage/gm", vec![OscType::Float(0.0)]);
        send("/livestage/go", vec![]);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut got = Vec::new();
        while got.is_empty() && std::time::Instant::now() < deadline {
            got.extend(hub.drain());
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        got.extend(hub.drain());
        assert_eq!(got.iter().map(|e| e.command.clone()).collect::<Vec<_>>(), [ShowCommand::Go { list: None }]);
    }

    #[test]
    fn sensitivity_classification() {
        use livestage_core::ShowCommand as C;
        for c in [C::Go { list: None }, C::GoCue { number: "1".into() }, C::StandbyNext, C::NextSlide, C::CartPad { cart: 0, pad: 0, down: true }, C::RequestStatus] {
            assert!(!c.is_sensitive(), "{c:?}");
        }
        for c in [
            C::Panic { hard: false },
            C::StopAll { fade: None },
            C::PauseAll,
            C::SetMasterGain { db: -10.0 },
            C::SetOutputsFullscreen { enabled: false },
            C::SetSubmaster { sub: 0, level: 1.0 },
            C::SetGrandMaster { level: 0.0 },
            C::SetBlackout { enabled: true },
            C::ClearProgrammer,
            C::TimecodeStop,
        ] {
            assert!(c.is_sensitive(), "{c:?}");
        }
    }
}
