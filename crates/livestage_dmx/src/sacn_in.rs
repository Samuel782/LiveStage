//! sACN (E1.31) reception and merging.
//!
//! A receiver thread joins the multicast group of every universe an input
//! patch uses (unicast to port 5568 works too) and merges the sources of
//! each universe the way E1.31 receivers do:
//!
//! - the highest priority wins the whole universe;
//! - sources of equal priority merge HTP, slot by slot;
//! - a source silent for 2.5 s (network data loss) or one that sends the
//!   stream-terminated flag is dropped at once;
//! - out-of-order packets (sequence 1..=20 behind) are discarded, preview
//!   packets ignored, and LiveStage's own output (same CID) never merged
//!   back in.
//!
//! The engine reads the merged universes and patches them onto its output
//! ([`livestage_core::dmx_config::InputPatch`]).

use crate::sacn::{self, DataPacket};
use crate::DmxError;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// E1.31 network data loss timeout.
pub const SOURCE_TIMEOUT: Duration = Duration::from_millis(2500);

/// The merge of one universe's sources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputUniverse {
    pub data: [u8; 512],
    /// Sources merged into `data` (the highest-priority ones).
    pub sources: u8,
    pub priority: u8,
}

#[derive(Clone, Debug)]
struct Source {
    cid: [u8; 16],
    priority: u8,
    sequence: u8,
    seen: Instant,
    data: [u8; 512],
    slots: usize,
}

/// Merge state for every received universe.
#[derive(Default)]
pub struct Merger {
    sources: HashMap<u16, Vec<Source>>,
}

impl Merger {
    /// Takes a packet; returns whether the universe's merge may have changed.
    pub fn packet(&mut self, p: &DataPacket, now: Instant) -> bool {
        if p.preview() {
            return false;
        }
        let list = self.sources.entry(p.universe).or_default();
        let existing = list.iter().position(|s| s.cid == p.cid);
        if p.terminated() {
            return existing.map(|i| list.remove(i)).is_some();
        }
        let source = match existing {
            Some(i) => {
                let s = &mut list[i];
                let behind = s.sequence.wrapping_sub(p.sequence);
                if (0..20).contains(&behind) {
                    return false; // duplicate or late
                }
                s
            }
            None => {
                list.push(Source { cid: p.cid, priority: 0, sequence: 0, seen: now, data: [0; 512], slots: 0 });
                list.last_mut().expect("just pushed")
            }
        };
        source.priority = p.priority;
        source.sequence = p.sequence;
        source.seen = now;
        source.slots = p.data.len().min(512);
        source.data[..source.slots].copy_from_slice(&p.data[..source.slots]);
        source.data[source.slots..].fill(0);
        true
    }

    /// Drops sources that went silent; returns the universes that changed.
    pub fn expire(&mut self, now: Instant) -> Vec<u16> {
        let mut changed = Vec::new();
        for (u, list) in &mut self.sources {
            let before = list.len();
            list.retain(|s| now.duration_since(s.seen) < SOURCE_TIMEOUT);
            if list.len() != before {
                changed.push(*u);
            }
        }
        changed
    }

    /// The merged universe, or `None` when no source is sending it.
    pub fn merged(&self, universe: u16) -> Option<InputUniverse> {
        let list = self.sources.get(&universe).filter(|l| !l.is_empty())?;
        let priority = list.iter().map(|s| s.priority).max()?;
        let mut out = InputUniverse { data: [0; 512], sources: 0, priority };
        for s in list.iter().filter(|s| s.priority == priority) {
            out.sources = out.sources.saturating_add(1);
            for (o, v) in out.data.iter_mut().zip(&s.data[..s.slots]) {
                *o = (*o).max(*v);
            }
        }
        Some(out)
    }
}

/// Merged input, shared with the engine.
#[derive(Default)]
pub struct InputShared {
    /// Universes with at least one live source.
    pub universes: Mutex<HashMap<u16, InputUniverse>>,
    /// Bumped whenever a merge changes (the engine renders on change).
    pub generation: AtomicU64,
    pub packets: AtomicU64,
}

/// The receiving thread.
pub struct SacnReceiver {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub shared: Arc<InputShared>,
}

impl SacnReceiver {
    /// Receives `universes` on the standard port, ignoring packets from
    /// `own_cid` (this LiveStage's output).
    pub fn start(universes: &[u16], own_cid: [u8; 16]) -> Result<Self, DmxError> {
        Self::start_on(sacn::SACN_PORT, universes, own_cid)
    }

    pub fn start_on(port: u16, universes: &[u16], own_cid: [u8; 16]) -> Result<Self, DmxError> {
        let socket = bind(port)?;
        for &u in universes {
            if let Err(e) = socket.join_multicast_v4(&sacn::multicast_addr(u), &std::net::Ipv4Addr::UNSPECIFIED) {
                tracing::warn!(universe = u, "sACN input: cannot join multicast group ({e}); unicast still works");
            }
        }
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let wanted: Vec<u16> = universes.to_vec();
        let running = Arc::new(AtomicBool::new(true));
        let shared = Arc::new(InputShared::default());
        let (r, sh) = (running.clone(), shared.clone());
        let handle = std::thread::Builder::new().name("livestage-sacn-in".into()).spawn(move || {
            let mut merger = Merger::default();
            let mut buf = [0u8; 1144];
            let mut last_sweep = Instant::now();
            while r.load(Ordering::Relaxed) {
                let mut changed: Vec<u16> = Vec::new();
                if let Ok(n) = socket.recv(&mut buf) {
                    let now = Instant::now();
                    if let Some(p) = sacn::decode_data_packet(&buf[..n]) {
                        if p.cid != own_cid && wanted.contains(&p.universe) {
                            sh.packets.fetch_add(1, Ordering::Relaxed);
                            if merger.packet(&p, now) {
                                changed.push(p.universe);
                            }
                        }
                    }
                }
                if last_sweep.elapsed() >= Duration::from_millis(100) {
                    last_sweep = Instant::now();
                    changed.extend(merger.expire(last_sweep));
                }
                if changed.is_empty() {
                    continue;
                }
                let mut universes = sh.universes.lock();
                let mut any = false;
                for u in changed {
                    let merged = merger.merged(u);
                    if universes.get(&u) != merged.as_ref() {
                        any = true;
                        match merged {
                            Some(m) => universes.insert(u, m),
                            None => universes.remove(&u),
                        };
                    }
                }
                if any {
                    sh.generation.fetch_add(1, Ordering::Release);
                }
            }
        })?;
        tracing::info!(?universes, port, "sACN input listening");
        Ok(Self { running, handle: Some(handle), shared })
    }
}

impl Drop for SacnReceiver {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Port 5568, shared with other sACN software on this machine.
fn bind(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    Ok(s.into())
}

/// Patches merged input onto output universe `index` (its slots in `out`).
pub fn apply_patches(patches: &[livestage_core::dmx_config::InputPatch], index: u16, input: &HashMap<u16, InputUniverse>, out: &mut [u8; 512]) {
    use livestage_core::dmx_config::InputMerge;
    for p in patches.iter().filter(|p| p.enabled && p.universe == index) {
        let Some(src) = input.get(&p.sacn_universe) else { continue };
        for k in 0..p.count as usize {
            let (from, to) = (p.from as usize + k, p.to as usize + k);
            if !(1..=512).contains(&from) || !(1..=512).contains(&to) {
                break;
            }
            let v = src.data[from - 1];
            let o = &mut out[to - 1];
            *o = match p.merge {
                InputMerge::Htp => (*o).max(v),
                InputMerge::Override => v,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sacn::{encode_data_packet, SacnSource};
    use livestage_core::dmx_config::{InputMerge, InputPatch};

    fn packet(cid: u8, universe: u16, priority: u8, seq: u8, data: &[u8]) -> Vec<u8> {
        encode_data_packet(&SacnSource { cid: [cid; 16], name: format!("src {cid}") }, universe, priority, seq, data)
    }

    fn feed(m: &mut Merger, bytes: &[u8], now: Instant) -> bool {
        m.packet(&sacn::decode_data_packet(bytes).unwrap(), now)
    }

    #[test]
    fn priority_then_htp_then_timeout() {
        let mut m = Merger::default();
        let t0 = Instant::now();
        feed(&mut m, &packet(1, 1, 100, 1, &[10, 200, 0]), t0);
        feed(&mut m, &packet(2, 1, 100, 1, &[50, 20]), t0);
        let u = m.merged(1).unwrap();
        assert_eq!((&u.data[..4], u.sources), (&[50, 200, 0, 0][..], 2), "equal priority merges HTP");
        // A higher-priority source takes the whole universe.
        feed(&mut m, &packet(3, 1, 150, 1, &[1]), t0 + Duration::from_millis(500));
        let u = m.merged(1).unwrap();
        assert_eq!((&u.data[..3], u.sources, u.priority), (&[1, 0, 0][..], 1, 150));
        // It goes silent: after the timeout the others are back.
        let later = t0 + Duration::from_millis(500) + SOURCE_TIMEOUT;
        feed(&mut m, &packet(1, 1, 100, 2, &[10, 200, 0]), later);
        feed(&mut m, &packet(2, 1, 100, 2, &[50, 20]), later);
        assert_eq!(m.expire(later), [1]);
        assert_eq!(&m.merged(1).unwrap().data[..2], &[50, 200]);
        assert!(m.merged(2).is_none());
    }

    #[test]
    fn sequence_termination_and_preview() {
        let mut m = Merger::default();
        let t = Instant::now();
        assert!(feed(&mut m, &packet(1, 7, 100, 10, &[100]), t));
        assert!(!feed(&mut m, &packet(1, 7, 100, 10, &[1]), t), "duplicate");
        assert!(!feed(&mut m, &packet(1, 7, 100, 5, &[1]), t), "late");
        assert!(feed(&mut m, &packet(1, 7, 100, 11, &[120]), t));
        assert!(feed(&mut m, &packet(1, 7, 100, 200, &[130]), t), "a big jump is a restarted sender");
        assert_eq!(m.merged(7).unwrap().data[0], 130);
        let mut preview = packet(1, 7, 100, 201, &[255]);
        preview[112] = 0x80;
        assert!(!feed(&mut m, &preview, t));
        let mut end = packet(1, 7, 100, 202, &[0]);
        end[112] = 0x40;
        assert!(feed(&mut m, &end, t));
        assert!(m.merged(7).is_none(), "terminated source dropped at once");
    }

    #[test]
    fn patches_ranges_with_htp_and_override() {
        let mut input = HashMap::new();
        let mut data = [0u8; 512];
        data[..4].copy_from_slice(&[10, 200, 30, 40]);
        input.insert(5, InputUniverse { data, sources: 1, priority: 100 });
        let mut out = [0u8; 512];
        out[..4].copy_from_slice(&[100, 100, 100, 100]);
        let htp = InputPatch { sacn_universe: 5, from: 1, count: 2, universe: 0, to: 1, merge: InputMerge::Htp, enabled: true };
        let over = InputPatch { from: 3, count: 2, to: 3, merge: InputMerge::Override, ..htp.clone() };
        let other = InputPatch { universe: 1, ..htp.clone() };
        let edge = InputPatch { from: 1, count: 100, to: 511, merge: InputMerge::Override, ..htp.clone() };
        apply_patches(&[htp, over, other, edge], 0, &input, &mut out);
        assert_eq!(&out[..4], &[100, 200, 30, 40]);
        assert_eq!(&out[510..], &[10, 200], "clipped at slot 512");
    }

    /// Real sockets: packets to the receiver's port are merged; LiveStage's
    /// own CID and universes nobody patched are ignored.
    #[test]
    fn receives_over_udp_and_ignores_itself() {
        let own = [9u8; 16];
        let port = 25_568 + (std::process::id() % 1000) as u16;
        let rx = SacnReceiver::start_on(port, &[3], own).unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let dest = ("127.0.0.1", port);
        tx.send_to(&packet(9, 3, 200, 1, &[255; 8]), dest).unwrap();
        tx.send_to(&packet(1, 4, 100, 1, &[255; 8]), dest).unwrap();
        tx.send_to(&packet(1, 3, 100, 1, &[7, 8, 9]), dest).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while rx.shared.generation.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let got = rx.shared.universes.lock().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(&got[&3].data[..4], &[7, 8, 9, 0]);
        assert_eq!(rx.shared.packets.load(Ordering::Relaxed), 1);
    }
}
