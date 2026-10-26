//! Timecode inputs (MIDI Time Code, Art-Net timecode) and MTC output.
//!
//! Decoders turn wire messages into [`TcSample`]s stamped with their
//! arrival time; the show thread feeds them to the core
//! [`TimecodeEngine`](livestage_core::timecode::TimecodeEngine), which owns
//! locking, drift compensation and slaving.
//!
//! ## MTC
//!
//! Eight quarter-frame messages (`F1 0nnn dddd`) carry one timecode over two
//! frames; piece *p* of a cycle labelled *F* is sent at *F + p/4* frames.
//! The decoder reports a position on every quarter frame once a full cycle
//! is known (≈100 samples/s at 25 fps), which gives the lock plenty to
//! average. Full-frame SysEx (`F0 7F dev 01 01 hh mm ss ff F7`) is a locate.
//!
//! ## Art-Net
//!
//! `OpTimeCode` (0x9700) packets carry one label per frame. A label repeated
//! by a stopped sender is reported as a locate.
//!
//! ## MTC out
//!
//! [`MtcGenerator`] turns the show's [`TcTransport`] into MTC from the show
//! thread: a full-frame message on every locate, start and stop, and while
//! rolling the quarter frame due at each quarter-frame boundary (piece *p*
//! of the cycle labelled *F* at *F + p/4*, cycles on even frames).

use crate::ControlError;
use crossbeam_channel::Sender;
use livestage_core::timecode::{FrameRate, SampleKind, TcSample, TcTransport, Timecode, TimecodeSource};
use livestage_core::ShowTime;
use midir::{Ignore, MidiInput, MidiInputConnection};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A decoded sample and when it arrived.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimecodeInput {
    pub at: Instant,
    pub origin: TimecodeSource,
    pub sample: TcSample,
}

fn sample_at(frames: f64, rate: FrameRate, kind: SampleKind) -> TcSample {
    let day = rate.day_length().as_micros() as f64;
    let us = rate.frames_to_micros(frames).rem_euclid(day);
    TcSample { position: ShowTime::from_micros(us.round() as u64), rate, kind }
}

// ======================================================================= MTC

/// MTC quarter-frame / full-frame decoder.
#[derive(Clone, Debug, Default)]
pub struct MtcDecoder {
    nibbles: [u8; 8],
    /// Bit `p` set = piece `p` received in the current run.
    have: u8,
    last_piece: Option<u8>,
    /// Frame count and rate of the last complete forward cycle (its piece-0 frame).
    base: Option<(u64, FrameRate)>,
}

impl MtcDecoder {
    fn assemble(&self) -> Option<Timecode> {
        let n = &self.nibbles;
        let rate = FrameRate::from_smpte_code((n[7] >> 1) & 0x03);
        Timecode::new(n[6] | (n[7] & 0x01) << 4, n[4] | (n[5] & 0x03) << 4, n[2] | (n[3] & 0x03) << 4, n[0] | (n[1] & 0x01) << 4, rate).ok()
    }

    /// Feeds the data byte of a quarter-frame message (`F1 <data>`).
    pub fn quarter_frame(&mut self, data: u8) -> Option<TcSample> {
        let piece = (data >> 4) & 0x07;
        let forward = self.last_piece.is_none_or(|p| piece == (p + 1) % 8);
        let reverse = self.last_piece.is_some_and(|p| (piece + 1) % 8 == p);
        if !forward {
            // Out of sequence: a dropped message, a jump or reverse play.
            self.base = None;
            if !reverse {
                self.have = 0;
            }
        }
        self.last_piece = Some(piece);
        self.nibbles[piece as usize] = data & 0x0F;
        self.have |= 1 << piece;
        if reverse {
            // Shuttling backwards: report where it is, but not as rolling.
            if piece == 0 && self.have == 0xFF {
                let tc = self.assemble()?;
                return Some(sample_at(tc.to_frames() as f64, tc.rate, SampleKind::Locate));
            }
            return None;
        }
        if piece == 7 && self.have == 0xFF {
            let tc = self.assemble()?;
            let frame = tc.to_frames();
            self.base = Some((frame, tc.rate));
            return Some(sample_at(frame as f64 + 1.75, tc.rate, SampleKind::Running));
        }
        // Inside the cycle after a known one: its label is base + 2 frames.
        let (base, rate) = self.base?;
        Some(sample_at(base as f64 + 2.0 + piece as f64 / 4.0, rate, SampleKind::Running))
    }

    /// Feeds a SysEx message; returns a locate for MTC full-frame messages.
    pub fn sysex(&mut self, bytes: &[u8]) -> Option<TcSample> {
        let tc = parse_full_frame(bytes)?;
        *self = Self::default();
        Some(sample_at(tc.to_frames() as f64, tc.rate, SampleKind::Locate))
    }

    /// Feeds any raw MIDI message.
    pub fn message(&mut self, bytes: &[u8]) -> Option<TcSample> {
        match bytes {
            [0xF1, data, ..] => self.quarter_frame(*data),
            [0xF0, ..] => self.sysex(bytes),
            _ => None,
        }
    }
}

/// Parses MTC full-frame SysEx: `F0 7F <device> 01 01 hh mm ss ff F7`.
pub fn parse_full_frame(b: &[u8]) -> Option<Timecode> {
    if b.len() != 10 || b[0] != 0xF0 || b[1] != 0x7F || b[3] != 0x01 || b[4] != 0x01 || b[9] != 0xF7 {
        return None;
    }
    let rate = FrameRate::from_smpte_code((b[5] >> 5) & 0x03);
    Timecode::new(b[5] & 0x1F, b[6] & 0x3F, b[7] & 0x3F, b[8] & 0x1F, rate).ok()
}

/// MTC full-frame SysEx for `tc` (device 0x7F = all).
pub fn encode_full_frame(tc: &Timecode, device: u8) -> [u8; 10] {
    [0xF0, 0x7F, device & 0x7F, 0x01, 0x01, tc.rate.smpte_code() << 5 | tc.hours, tc.minutes, tc.seconds, tc.frames, 0xF7]
}

/// Data bytes of the eight quarter-frame messages for `tc` (send each as `F1 <byte>`).
pub fn encode_quarter_frames(tc: &Timecode) -> [u8; 8] {
    let nib = [
        tc.frames & 0x0F,
        tc.frames >> 4,
        tc.seconds & 0x0F,
        tc.seconds >> 4,
        tc.minutes & 0x0F,
        tc.minutes >> 4,
        tc.hours & 0x0F,
        (tc.hours >> 4) & 0x01 | tc.rate.smpte_code() << 1,
    ];
    std::array::from_fn(|p| (p as u8) << 4 | nib[p])
}

/// An open MTC input. Dropping it closes the port.
pub struct MtcInput {
    _conn: MidiInputConnection<MtcDecoder>,
    pub port_name: String,
}

impl MtcInput {
    /// Opens the named MIDI input (or the first available one) for MTC.
    pub fn open(port_name: Option<&str>, tx: Sender<TimecodeInput>) -> Result<Self, ControlError> {
        let mut input = MidiInput::new("LiveStage MTC").map_err(|e| ControlError::Midi(e.to_string()))?;
        // Keep time code (and SysEx for full-frame locates); drop active sensing.
        input.ignore(Ignore::ActiveSense);
        let ports = input.ports();
        let port = ports
            .iter()
            .find(|p| port_name.is_none_or(|want| input.port_name(p).is_ok_and(|n| n == want)))
            .ok_or_else(|| ControlError::Midi(format!("no MIDI input {}", port_name.unwrap_or("available"))))?
            .clone();
        let name = input.port_name(&port).unwrap_or_default();
        let conn = input
            .connect(
                &port,
                "livestage-mtc",
                move |_stamp, bytes, decoder: &mut MtcDecoder| {
                    let at = Instant::now();
                    if let Some(sample) = decoder.message(bytes) {
                        let _ = tx.send(TimecodeInput { at, origin: TimecodeSource::Mtc, sample });
                    }
                },
                MtcDecoder::default(),
            )
            .map_err(|e| ControlError::Midi(e.to_string()))?;
        tracing::info!(port = %name, "MTC input open");
        Ok(Self { _conn: conn, port_name: name })
    }
}

// =================================================================== MTC out

/// Schedules MTC messages for a transport (no I/O; see [`MtcOutput`]).
#[derive(Clone, Debug, Default)]
pub struct MtcGenerator {
    /// Last quarter frame sent (quarter frames since `00:00:00:00`).
    sent: Option<i64>,
    running: bool,
    generation: Option<u64>,
    rate: Option<FrameRate>,
}

impl MtcGenerator {
    /// Emits the messages due for `t` through `send`. Call every show tick:
    /// at 1 kHz quarter frames (every 10 ms at 25 fps) leave on time.
    pub fn update(&mut self, t: &TcTransport, mut send: impl FnMut(&[u8])) {
        let frames = t.rate.micros_to_frames(t.position.as_micros() as f64);
        let jumped = self.generation != Some(t.generation) || self.rate != Some(t.rate);
        self.generation = Some(t.generation);
        self.rate = Some(t.rate);
        if !t.running {
            if self.running || jumped {
                // Parked: tell receivers where.
                send(&encode_full_frame(&Timecode::from_frames(frames.max(0.0) as u64 % t.rate.frames_per_day(), t.rate), 0x7F));
            }
            self.running = false;
            self.sent = None;
            return;
        }
        let q = (frames * 4.0).floor() as i64;
        match self.sent {
            // Rolling on: every quarter frame since the last one (a late
            // tick catches up within one cycle).
            Some(last) if self.running && !jumped && (last..last + 8).contains(&q) => {
                for k in last + 1..=q {
                    send(&[0xF1, quarter_frame(k, t.rate)]);
                }
            }
            // Started, located, or too far behind: a full frame first.
            _ => send(&encode_full_frame(&Timecode::from_frames((q / 4).max(0) as u64 % t.rate.frames_per_day(), t.rate), 0x7F)),
        }
        self.sent = Some(q);
        self.running = true;
    }
}

/// Data byte of quarter frame `k` (counted from `00:00:00:00`).
fn quarter_frame(k: i64, rate: FrameRate) -> u8 {
    let piece = k.rem_euclid(8) as usize;
    let cycle_frame = (k - piece as i64) / 4;
    let label = Timecode::from_frames(cycle_frame.max(0) as u64 % rate.frames_per_day(), rate);
    encode_quarter_frames(&label)[piece]
}

/// An open MTC output port driven by an [`MtcGenerator`].
pub struct MtcOutput {
    conn: midir::MidiOutputConnection,
    generator: MtcGenerator,
    pub port_name: String,
    errors: u64,
}

impl MtcOutput {
    /// Opens the named MIDI output (or the first available one).
    pub fn open(port_name: Option<&str>) -> Result<Self, ControlError> {
        let out = midir::MidiOutput::new("LiveStage MTC").map_err(|e| ControlError::Midi(e.to_string()))?;
        let ports = out.ports();
        let port = ports
            .iter()
            .find(|p| port_name.is_none_or(|want| out.port_name(p).is_ok_and(|n| n == want)))
            .ok_or_else(|| ControlError::Midi(format!("no MIDI output {}", port_name.unwrap_or("available"))))?
            .clone();
        let name = out.port_name(&port).unwrap_or_default();
        let conn = out.connect(&port, "livestage-mtc-out").map_err(|e| ControlError::Midi(e.to_string()))?;
        tracing::info!(port = %name, "MTC output open");
        Ok(Self { conn, generator: MtcGenerator::default(), port_name: name, errors: 0 })
    }

    /// Sends what is due for `t` (call every show tick).
    pub fn update(&mut self, t: &TcTransport) {
        let (conn, errors) = (&mut self.conn, &mut self.errors);
        self.generator.update(t, |msg| {
            if let Err(e) = conn.send(msg) {
                *errors += 1;
                if errors.is_power_of_two() {
                    tracing::warn!("MTC output: {e} ({errors} failed sends)");
                }
            }
        });
    }
}

// =================================================================== Art-Net

pub const OP_TIMECODE: u16 = 0x9700;
const ARTNET_ID: &[u8; 8] = b"Art-Net\0";

/// Parses an `OpTimeCode` packet into its label and stream id.
pub fn parse_art_timecode(p: &[u8]) -> Option<(Timecode, u8)> {
    if p.len() < 19 || &p[..8] != ARTNET_ID || u16::from_le_bytes([p[8], p[9]]) != OP_TIMECODE {
        return None;
    }
    let rate = FrameRate::from_smpte_code(p[18]);
    Some((Timecode::new(p[17], p[16], p[15], p[14], rate).ok()?, p[13]))
}

/// Encodes an `OpTimeCode` packet (Art-Net 4).
pub fn encode_art_timecode(tc: &Timecode, stream: u8) -> [u8; 19] {
    let mut p = [0u8; 19];
    p[..8].copy_from_slice(ARTNET_ID);
    p[8..10].copy_from_slice(&OP_TIMECODE.to_le_bytes());
    p[10..12].copy_from_slice(&14u16.to_be_bytes());
    p[13] = stream;
    p[14] = tc.frames;
    p[15] = tc.seconds;
    p[16] = tc.minutes;
    p[17] = tc.hours;
    p[18] = tc.rate.smpte_code();
    p
}

/// Turns Art-Net timecode packets into samples. A label repeated twice
/// more (a stopped sender refreshing its position) becomes a locate; a
/// single duplicate (network) is ignored.
#[derive(Clone, Debug, Default)]
pub struct ArtNetTimecodeDecoder {
    last: Option<Timecode>,
    repeats: u32,
}

impl ArtNetTimecodeDecoder {
    pub fn packet(&mut self, p: &[u8]) -> Option<TcSample> {
        let (tc, _stream) = parse_art_timecode(p)?;
        if self.last == Some(tc) {
            self.repeats += 1;
            return (self.repeats == 2).then(|| sample_at(tc.to_frames() as f64, tc.rate, SampleKind::Locate));
        }
        self.last = Some(tc);
        self.repeats = 0;
        Some(sample_at(tc.to_frames() as f64, tc.rate, SampleKind::Running))
    }
}

/// UDP listener for Art-Net timecode. The socket is shared (SO_REUSEADDR /
/// SO_REUSEPORT) so lighting software on the same machine keeps port 6454.
pub struct ArtNetTimecodeListener {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub port: u16,
}

fn shared_udp_socket(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    Ok(s.into())
}

impl ArtNetTimecodeListener {
    pub fn start(port: u16, tx: Sender<TimecodeInput>) -> Result<Self, ControlError> {
        let socket = shared_udp_socket(port)?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let port = socket.local_addr()?.port();
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let handle = std::thread::Builder::new().name("livestage-artnet-tc".into()).spawn(move || {
            let mut buf = [0u8; 1024];
            let mut decoder = ArtNetTimecodeDecoder::default();
            while r.load(Ordering::Relaxed) {
                let Ok((n, _peer)) = socket.recv_from(&mut buf) else { continue };
                let at = Instant::now();
                if let Some(sample) = decoder.packet(&buf[..n]) {
                    if tx.send(TimecodeInput { at, origin: TimecodeSource::ArtNet, sample }).is_err() {
                        break;
                    }
                }
            }
        })?;
        tracing::info!(port, "Art-Net timecode listening");
        Ok(Self { running, handle: Some(handle), port })
    }
}

impl Drop for ArtNetTimecodeListener {
    /// Signals the thread and returns at once (it exits within its 200 ms
    /// read timeout); the shared socket lets a replacement bind meanwhile.
    /// Not joining keeps a source change from stalling the show thread.
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.handle.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestage_core::timecode::{LockConfig, TimecodeLock};

    fn tc(s: &str) -> Timecode {
        s.parse().unwrap()
    }

    #[test]
    fn quarter_frames_roundtrip_and_report_every_piece() {
        let mut d = MtcDecoder::default();
        let start = tc("01:02:03:04");
        // First cycle: nothing until piece 7, then label + 1.75 frames.
        let first: Vec<_> = encode_quarter_frames(&start).iter().map(|&b| d.quarter_frame(b)).collect();
        assert!(first[..7].iter().all(Option::is_none));
        let s = first[7].unwrap();
        assert_eq!(s.kind, SampleKind::Running);
        assert_eq!(s.rate, FrameRate::Fps25);
        assert_eq!(s.position, start.to_time() + ShowTime::from_millis(70));
        // Next cycle (label + 2 frames): a sample per quarter frame, 10 ms apart.
        let next = start.add_frames(2);
        let samples: Vec<_> = encode_quarter_frames(&next).iter().map(|&b| d.quarter_frame(b).unwrap()).collect();
        for (p, s) in samples.iter().enumerate() {
            assert_eq!(s.position, next.to_time() + ShowTime::from_millis(10 * p as u64), "piece {p}");
        }
    }

    #[test]
    fn rate_bits_and_drop_frame() {
        let df = Timecode::new(23, 59, 59, 29, FrameRate::Fps2997Df).unwrap();
        let mut d = MtcDecoder::default();
        let s = encode_quarter_frames(&df).iter().filter_map(|&b| d.quarter_frame(b)).last().unwrap();
        assert_eq!(s.rate, FrameRate::Fps2997Df);
        // 23:59:59;29 + 1.75 frames wraps past midnight.
        assert!(s.position < FrameRate::Fps2997Df.frame_start(1));
    }

    #[test]
    fn dropped_quarter_frame_resyncs() {
        let mut d = MtcDecoder::default();
        let a = tc("00:00:10:00");
        encode_quarter_frames(&a).iter().for_each(|&b| {
            d.quarter_frame(b);
        });
        let b = encode_quarter_frames(&a.add_frames(2));
        assert!(d.quarter_frame(b[0]).is_some());
        // Piece 1 lost: no samples until a fresh complete cycle.
        assert!(b[2..].iter().all(|&x| d.quarter_frame(x).is_none()));
        let c = a.add_frames(4);
        let got: Vec<_> = encode_quarter_frames(&c).iter().filter_map(|&x| d.quarter_frame(x)).collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].position, c.to_time() + ShowTime::from_millis(70));
    }

    #[test]
    fn reverse_play_reports_locates_only() {
        let mut d = MtcDecoder::default();
        let t = tc("00:00:20:00");
        encode_quarter_frames(&t).iter().for_each(|&b| {
            d.quarter_frame(b);
        });
        let back = encode_quarter_frames(&t.add_frames(-2));
        let got: Vec<_> = back.iter().rev().filter_map(|&b| d.quarter_frame(b)).collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, SampleKind::Locate);
        assert_eq!(got[0].position, t.add_frames(-2).to_time());
    }

    #[test]
    fn full_frame_locate() {
        let t = Timecode::new(10, 20, 30, 12, FrameRate::Fps30).unwrap();
        let msg = encode_full_frame(&t, 0x7F);
        assert_eq!(parse_full_frame(&msg), Some(t));
        let s = MtcDecoder::default().message(&msg).unwrap();
        assert_eq!(s.kind, SampleKind::Locate);
        assert_eq!(s.position, t.to_time());
        assert!(parse_full_frame(&msg[..9]).is_none());
    }

    #[test]
    fn art_timecode_roundtrip_and_stationary_detection() {
        let t = Timecode::new(1, 0, 0, 5, FrameRate::Fps24).unwrap();
        let p = encode_art_timecode(&t, 3);
        assert_eq!(parse_art_timecode(&p), Some((t, 3)));
        let mut bad = p;
        bad[9] = 0x50; // OpDmx
        assert!(parse_art_timecode(&bad).is_none());

        let mut d = ArtNetTimecodeDecoder::default();
        assert_eq!(d.packet(&p).unwrap().kind, SampleKind::Running);
        assert!(d.packet(&p).is_none(), "one duplicate is ignored");
        assert_eq!(d.packet(&p).unwrap().kind, SampleKind::Locate);
        assert!(d.packet(&p).is_none());
        let n = encode_art_timecode(&t.add_frames(1), 3);
        assert_eq!(d.packet(&n).unwrap().position, t.add_frames(1).to_time());
    }

    /// End to end: a simulated MTC sender (0.05 % slow, jittery arrival)
    /// through the decoder into the lock tracks the sender within a frame.
    #[test]
    fn mtc_stream_locks_with_drift() {
        let mut d = MtcDecoder::default();
        let mut lock = TimecodeLock::new(LockConfig::default());
        let start = tc("09:59:58:00");
        let rate = FrameRate::Fps25;
        let qf_us = rate.frames_to_micros(0.25) / 0.9995;
        let mut label = start;
        let mut k = 0u64;
        let mut seed = 1u64;
        for _cycle in 0..500 {
            for b in encode_quarter_frames(&label) {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let jitter = (seed >> 54) as f64; // 0..1023 µs
                let arrival = ShowTime::from_micros((k as f64 * qf_us + jitter) as u64);
                if let Some(s) = d.quarter_frame(b) {
                    lock.feed(arrival, s);
                }
                lock.update(arrival);
                k += 1;
            }
            label = label.add_frames(2);
        }
        let now = ShowTime::from_micros((k as f64 * qf_us) as u64);
        let t = lock.transport(now);
        assert!(t.running);
        // Truth: the sender is at piece k of its stream.
        let truth = start.to_time().as_micros() as f64 + rate.frames_to_micros(k as f64 / 4.0);
        let err = t.position.as_micros() as f64 - truth;
        assert!(err.abs() < rate.frames_to_micros(0.5), "within half a frame: {err} µs");
        assert!((t.drift_ppm() + 500.0).abs() < 200.0, "measured drift {} ppm", t.drift_ppm());
        // 500 cycles = 1000 frames = 40 s after the start label.
        assert_eq!(lock.transport(now + ShowTime::from_millis(20)).timecode().to_string(), "10:00:38:00");
    }

    /// MTC out read back by the MTC decoder: positions agree within a
    /// quarter frame while rolling; locates and stops send full frames.
    #[test]
    fn generated_mtc_decodes_to_the_transport() {
        use livestage_core::timecode::TcTransport;
        for rate in [FrameRate::Fps25, FrameRate::Fps2997Df, FrameRate::Fps24] {
            let start = tc("00:59:59:00").with_rate(rate).to_time();
            let mut gen = MtcGenerator::default();
            let mut dec = MtcDecoder::default();
            let mut t = TcTransport { position: start, rate, running: true, generation: 1, ..TcTransport::default() };
            let (mut full, mut checked) = (0, 0);
            for ms in 0..3_000u64 {
                t.position = start + ShowTime::from_millis(ms);
                let mut msgs = Vec::new();
                gen.update(&t, |m| msgs.push(m.to_vec()));
                for m in msgs {
                    if m[0] == 0xF0 {
                        full += 1;
                    }
                    if let Some(s) = dec.message(&m) {
                        if s.kind == SampleKind::Running {
                            let err = s.position.as_micros() as f64 - t.position.as_micros() as f64;
                            assert!(err.abs() <= rate.frame_duration().as_micros() as f64 / 4.0 + 1_000.0, "{rate:?} at {ms} ms: {err} µs");
                            checked += 1;
                        }
                    }
                }
            }
            assert_eq!(full, 1, "{rate:?}: one full frame at the start");
            assert!(checked as f64 > rate.fps() * 4.0 * 2.5, "{rate:?}: {checked} quarter frames decoded");
            // A locate is a new generation: full frame, then rolling again.
            t.generation = 2;
            t.position = tc("02:00:00:00").with_rate(rate).to_time();
            let mut msgs = Vec::new();
            gen.update(&t, |m| msgs.push(m.to_vec()));
            assert_eq!(msgs.len(), 1);
            assert_eq!(parse_full_frame(&msgs[0]).unwrap().hours, 2);
            // Stopping parks with a full frame, once.
            t.running = false;
            let mut n = 0;
            gen.update(&t, |_| n += 1);
            gen.update(&t, |_| n += 1);
            assert_eq!(n, 1);
        }
    }
}
