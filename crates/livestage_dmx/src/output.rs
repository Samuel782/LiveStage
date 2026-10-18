//! Paced network transmitter for Art-Net and sACN.

use crate::universe::Universe;
use crate::{artnet, sacn, DmxError};
use livestage_core::dmx_config::{DmxConfig, DmxTransport};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Latest frame, written by the show thread, read by the sender thread.
pub type SharedFrame = Arc<Mutex<BTreeMap<u16, Universe>>>;

#[derive(Debug, Clone)]
struct Route {
    index: u16,
    kind: RouteKind,
    dest: SocketAddr,
    seq: u8,
}

#[derive(Debug, Clone)]
enum RouteKind {
    ArtNet { port_address: u16 },
    Sacn { universe: u16, priority: u8 },
}

fn parse_dest(s: &str, default_port: u16) -> Result<SocketAddr, DmxError> {
    let with_port = if s.contains(':') { s.to_string() } else { format!("{s}:{default_port}") };
    with_port.to_socket_addrs().ok().and_then(|mut a| a.next()).ok_or_else(|| DmxError::BadAddress(s.to_string()))
}

pub struct DmxOutput {
    frame: SharedFrame,
    running: Arc<AtomicBool>,
    pub packets_sent: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl DmxOutput {
    /// Starts the sender. Universes with `Disabled` transport are not sent.
    pub fn start(config: &DmxConfig, frame: SharedFrame) -> Result<Self, DmxError> {
        let mut routes = Vec::new();
        for u in &config.universes {
            match &u.transport {
                DmxTransport::ArtNet { port_address, destination } => routes.push(Route {
                    index: u.index,
                    kind: RouteKind::ArtNet { port_address: *port_address },
                    dest: parse_dest(destination, artnet::ARTNET_PORT)?,
                    seq: 1,
                }),
                DmxTransport::Sacn { universe, priority, destination } => {
                    let dest = match destination {
                        Some(d) => parse_dest(d, sacn::SACN_PORT)?,
                        None => SocketAddr::from((sacn::multicast_addr(*universe), sacn::SACN_PORT)),
                    };
                    routes.push(Route {
                        index: u.index,
                        kind: RouteKind::Sacn { universe: *universe, priority: *priority },
                        dest,
                        seq: 0,
                    });
                }
                DmxTransport::Disabled => {}
            }
        }
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_broadcast(true)?;
        socket.set_multicast_ttl_v4(4)?;
        let running = Arc::new(AtomicBool::new(true));
        let packets_sent = Arc::new(AtomicU64::new(0));
        let period = Duration::from_secs_f32(1.0 / config.refresh_hz.clamp(1.0, 44.0));
        let source = sacn::SacnSource::new("LiveStage");

        let (frame2, running2, sent2) = (frame.clone(), running.clone(), packets_sent.clone());
        let handle = std::thread::Builder::new().name("livestage-dmx-out".into()).spawn(move || {
            let mut next = Instant::now();
            while running2.load(Ordering::Relaxed) {
                let snapshot = frame2.lock().clone();
                for r in &mut routes {
                    let data = snapshot.get(&r.index).map(|u| u.data).unwrap_or([0; 512]);
                    let packet = match r.kind {
                        RouteKind::ArtNet { port_address } => {
                            r.seq = if r.seq == 255 { 1 } else { r.seq + 1 }; // 0 disables sequencing
                            artnet::encode_art_dmx(r.seq, 0, port_address, &data)
                        }
                        RouteKind::Sacn { universe, priority } => {
                            r.seq = r.seq.wrapping_add(1);
                            sacn::encode_data_packet(&source, universe, priority, r.seq, &data)
                        }
                    };
                    match socket.send_to(&packet, r.dest) {
                        Ok(_) => {
                            sent2.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => tracing::debug!(dest = %r.dest, "dmx send failed: {e}"),
                    }
                }
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        })?;
        Ok(Self { frame, running, packets_sent, handle: Some(handle) })
    }

    pub fn frame(&self) -> &SharedFrame {
        &self.frame
    }
}

impl Drop for DmxOutput {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
