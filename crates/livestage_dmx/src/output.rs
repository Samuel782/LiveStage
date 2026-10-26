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

/// Builds each frame's packets, in sending order: every universe's data
/// first, then the synchronisation packets that make nodes apply them
/// together (E1.31 sync on the sync universe, ArtSync).
#[derive(Debug)]
pub(crate) struct FramePlan {
    routes: Vec<Route>,
    source: sacn::SacnSource,
    /// sACN synchronisation address, with where its sync packets go.
    sacn_sync: Option<(u16, Vec<SocketAddr>)>,
    sync_seq: u8,
    /// Where ArtSync goes (wherever ArtDmx goes), when enabled.
    artsync: Vec<SocketAddr>,
}

impl FramePlan {
    pub(crate) fn new(config: &DmxConfig, source: sacn::SacnSource) -> Result<Self, DmxError> {
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
                    routes.push(Route { index: u.index, kind: RouteKind::Sacn { universe: *universe, priority: *priority }, dest, seq: 0 });
                }
                DmxTransport::Disabled => {}
            }
        }
        let distinct = |want_sacn: bool| {
            let mut dests: Vec<SocketAddr> = Vec::new();
            for r in routes.iter().filter(|r| matches!(r.kind, RouteKind::Sacn { .. }) == want_sacn) {
                // Multicast sACN data is synchronised on the sync universe's
                // own group; unicast receivers get the sync packet directly.
                let multicast = want_sacn && r.dest.ip().is_multicast();
                if !multicast && !dests.contains(&r.dest) {
                    dests.push(r.dest);
                }
            }
            dests
        };
        let sacn_routes = routes.iter().any(|r| matches!(r.kind, RouteKind::Sacn { .. }));
        let sacn_sync = match config.sync.sacn_sync_universe {
            Some(address) if !(1..=sacn::MAX_UNIVERSE).contains(&address) => {
                tracing::warn!(address, "sACN synchronisation universe must be 1–63999; sending unsynchronised");
                None
            }
            Some(address) if sacn_routes => {
                let mut dests = distinct(true);
                if routes.iter().any(|r| matches!(r.kind, RouteKind::Sacn { .. }) && r.dest.ip().is_multicast()) {
                    dests.insert(0, SocketAddr::from((sacn::multicast_addr(address), sacn::SACN_PORT)));
                }
                Some((address, dests))
            }
            _ => None,
        };
        let artsync = if config.sync.artsync { distinct(false) } else { Vec::new() };
        Ok(Self { routes, source, sacn_sync, sync_seq: 0, artsync })
    }

    /// The packets of one frame, in sending order.
    pub(crate) fn frame(&mut self, snapshot: &BTreeMap<u16, Universe>, out: &mut Vec<(Vec<u8>, SocketAddr)>) {
        out.clear();
        let sync_address = self.sacn_sync.as_ref().map_or(0, |(a, _)| *a);
        // Phase 1: the data, which synchronised receivers buffer.
        for r in &mut self.routes {
            let data = snapshot.get(&r.index).map(|u| u.data).unwrap_or([0; 512]);
            let packet = match r.kind {
                RouteKind::ArtNet { port_address } => {
                    r.seq = if r.seq == 255 { 1 } else { r.seq + 1 }; // 0 disables sequencing
                    artnet::encode_art_dmx(r.seq, 0, port_address, &data)
                }
                RouteKind::Sacn { universe, priority } => {
                    r.seq = r.seq.wrapping_add(1);
                    sacn::encode_data_packet(&self.source, universe, priority, r.seq, sync_address, &data)
                }
            };
            out.push((packet, r.dest));
        }
        // Phase 2: one trigger per protocol, so every universe changes at once.
        if let Some((address, dests)) = &self.sacn_sync {
            self.sync_seq = self.sync_seq.wrapping_add(1);
            let packet = sacn::encode_sync_packet(&self.source, self.sync_seq, *address);
            out.extend(dests.iter().map(|d| (packet.clone(), *d)));
        }
        let art_sync = artnet::encode_art_sync();
        out.extend(self.artsync.iter().map(|d| (art_sync.to_vec(), *d)));
    }
}

pub struct DmxOutput {
    frame: SharedFrame,
    running: Arc<AtomicBool>,
    pub packets_sent: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl DmxOutput {
    /// Starts the sender, identified on sACN as `source`. Universes with
    /// `Disabled` transport are not sent.
    pub fn start(config: &DmxConfig, frame: SharedFrame, source: sacn::SacnSource) -> Result<Self, DmxError> {
        let mut plan = FramePlan::new(config, source)?;
        if let Some((address, dests)) = &plan.sacn_sync {
            tracing::info!(address, destinations = dests.len(), "sACN synchronisation on");
        }
        if !plan.artsync.is_empty() {
            tracing::info!(destinations = plan.artsync.len(), "ArtSync on");
        }
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_broadcast(true)?;
        socket.set_multicast_ttl_v4(4)?;
        let running = Arc::new(AtomicBool::new(true));
        let packets_sent = Arc::new(AtomicU64::new(0));
        let period = Duration::from_secs_f32(1.0 / config.refresh_hz.clamp(1.0, 44.0));

        let (frame2, running2, sent2) = (frame.clone(), running.clone(), packets_sent.clone());
        let handle = std::thread::Builder::new().name("livestage-dmx-out".into()).spawn(move || {
            let mut next = Instant::now();
            let mut packets = Vec::new();
            while running2.load(Ordering::Relaxed) {
                // One consistent snapshot per frame: the show thread publishes
                // all universes of a render under this lock.
                let snapshot = frame2.lock().clone();
                plan.frame(&snapshot, &mut packets);
                for (packet, dest) in &packets {
                    match socket.send_to(packet, dest) {
                        Ok(_) => {
                            sent2.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => tracing::debug!(%dest, "dmx send failed: {e}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use livestage_core::dmx_config::{DmxSyncConfig, UniverseConfig};

    fn universe(index: u16, transport: DmxTransport) -> UniverseConfig {
        UniverseConfig { index, name: format!("U{index}"), transport }
    }

    fn sacn(universe: u16, destination: Option<&str>) -> DmxTransport {
        DmxTransport::Sacn { universe, priority: 100, destination: destination.map(String::from) }
    }

    fn artnet(port_address: u16, destination: &str) -> DmxTransport {
        DmxTransport::ArtNet { port_address, destination: destination.into() }
    }

    fn config(universes: Vec<UniverseConfig>, sync: DmxSyncConfig) -> DmxConfig {
        DmxConfig { universes, sync, ..DmxConfig::default() }
    }

    fn source() -> sacn::SacnSource {
        sacn::SacnSource { cid: [5; 16], name: "LiveStage".into() }
    }

    fn frame_of(plan: &mut FramePlan) -> Vec<(Vec<u8>, SocketAddr)> {
        let mut out = Vec::new();
        plan.frame(&BTreeMap::new(), &mut out);
        out
    }

    #[test]
    fn unsynchronised_by_default() {
        let cfg = config(vec![universe(0, sacn(1, None)), universe(1, artnet(0, "10.0.0.9"))], DmxSyncConfig::default());
        let packets = frame_of(&mut FramePlan::new(&cfg, source()).unwrap());
        assert_eq!(packets.len(), 2, "data only");
        assert_eq!(sacn::decode_data_packet(&packets[0].0).unwrap().sync_address, 0);
    }

    #[test]
    fn sacn_data_then_one_sync_per_frame() {
        let sync = DmxSyncConfig { sacn_sync_universe: Some(7000), artsync: false };
        let cfg = config(
            vec![universe(0, sacn(1, None)), universe(1, sacn(2, None)), universe(2, sacn(3, Some("10.0.0.5")))],
            sync,
        );
        let mut plan = FramePlan::new(&cfg, source()).unwrap();
        for frame in 1..=3u8 {
            let packets = frame_of(&mut plan);
            // Phase 1: every universe, each naming the sync address.
            for (i, (p, _)) in packets[..3].iter().enumerate() {
                let d = sacn::decode_data_packet(p).unwrap();
                assert_eq!((d.universe, d.sync_address, d.sequence), (i as u16 + 1, 7000, frame));
            }
            // Phase 2: the sync packet on the sync universe's multicast group,
            // and directly to the unicast receiver.
            let syncs: Vec<_> = packets[3..].iter().map(|(p, d)| (sacn::decode_sync_packet(p).unwrap(), *d)).collect();
            let multicast: SocketAddr = "239.255.27.88:5568".parse().unwrap(); // 7000 = 0x1B58
            let unicast: SocketAddr = "10.0.0.5:5568".parse().unwrap();
            assert_eq!(syncs, [(([5; 16], frame, 7000), multicast), (([5; 16], frame, 7000), unicast)]);
        }
    }

    #[test]
    fn artsync_follows_artdmx_to_each_destination_once() {
        let sync = DmxSyncConfig { sacn_sync_universe: Some(9), artsync: true };
        let cfg = config(
            vec![
                universe(0, artnet(0, "2.255.255.255")),
                universe(1, artnet(1, "2.255.255.255")),
                universe(2, artnet(2, "2.0.0.7")),
                universe(3, sacn(4, Some("10.0.0.5"))),
            ],
            sync,
        );
        let packets = frame_of(&mut FramePlan::new(&cfg, source()).unwrap());
        let kinds: Vec<String> = packets
            .iter()
            .map(|(p, d)| {
                let kind = if p.starts_with(b"Art-Net\0") {
                    if u16::from_le_bytes([p[8], p[9]]) == artnet::OP_SYNC { "ArtSync" } else { "ArtDmx" }
                } else if sacn::decode_sync_packet(p).is_some() {
                    "E1.31 sync"
                } else {
                    "E1.31 data"
                };
                format!("{kind} → {d}")
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "ArtDmx → 2.255.255.255:6454",
                "ArtDmx → 2.255.255.255:6454",
                "ArtDmx → 2.0.0.7:6454",
                "E1.31 data → 10.0.0.5:5568",
                "E1.31 sync → 10.0.0.5:5568",
                "ArtSync → 2.255.255.255:6454",
                "ArtSync → 2.0.0.7:6454",
            ]
        );
    }

    #[test]
    fn invalid_sync_universe_is_ignored() {
        for bad in [0, 64_000] {
            let sync = DmxSyncConfig { sacn_sync_universe: Some(bad), artsync: false };
            let packets = frame_of(&mut FramePlan::new(&config(vec![universe(0, sacn(1, None))], sync), source()).unwrap());
            assert_eq!(packets.len(), 1);
            assert_eq!(sacn::decode_data_packet(&packets[0].0).unwrap().sync_address, 0);
        }
        // No sACN universes: nothing to synchronise.
        let sync = DmxSyncConfig { sacn_sync_universe: Some(9), artsync: false };
        assert_eq!(frame_of(&mut FramePlan::new(&config(vec![universe(0, artnet(0, "2.0.0.7"))], sync), source()).unwrap()).len(), 1);
    }

    /// The running sender over loopback: each frame's data from one
    /// snapshot, then its sync packets, in that order.
    #[test]
    fn sends_frames_in_two_phases_over_udp() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let at = rx.local_addr().unwrap().to_string();
        let sync = DmxSyncConfig { sacn_sync_universe: Some(500), artsync: true };
        let mut cfg = config(vec![universe(0, sacn(1, Some(&at))), universe(1, sacn(2, Some(&at))), universe(2, artnet(0, &at))], sync);
        cfg.refresh_hz = 44.0;
        let frame: SharedFrame = Default::default();
        {
            let mut f = frame.lock();
            for i in 0..3u16 {
                f.insert(i, Universe { data: [i as u8 + 10; 512] });
            }
        }
        let out = DmxOutput::start(&cfg, frame, source()).unwrap();
        let mut buf = [0u8; 1024];
        let mut seen = Vec::new();
        while seen.len() < 15 {
            let n = rx.recv(&mut buf).unwrap();
            let p = &buf[..n];
            seen.push(if let Some(d) = sacn::decode_data_packet(p) {
                assert_eq!(d.sync_address, 500);
                format!("sacn {} = {}", d.universe, d.data[0])
            } else if sacn::decode_sync_packet(p).is_some() {
                "sacn sync".into()
            } else if u16::from_le_bytes([p[8], p[9]]) == artnet::OP_SYNC {
                "artsync".into()
            } else {
                format!("artdmx = {}", p[18])
            });
        }
        drop(out);
        let one_frame = ["sacn 1 = 10", "sacn 2 = 11", "artdmx = 12", "sacn sync", "artsync"];
        assert_eq!(seen, one_frame.repeat(3));
    }
}
