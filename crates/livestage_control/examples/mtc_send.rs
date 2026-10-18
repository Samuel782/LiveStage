//! MTC test generator: streams MIDI Time Code from a virtual MIDI source
//! (macOS / Linux) or an existing output port.
//!
//! ```text
//! cargo run -p livestage_control --example mtc_send -- [start TC] [seconds] [fps] [port]
//! cargo run -p livestage_control --example mtc_send -- 00:59:58:00 10 25
//! ```
//!
//! Without `port` a virtual source called "LiveStage MTC Test" is created;
//! pick it as the MTC input in Settings → Timecode. A full-frame locate is
//! sent first, then quarter frames at the real rate.

use livestage_control::timecode::{encode_full_frame, encode_quarter_frames};
use livestage_core::timecode::{FrameRate, Timecode};
use midir::{MidiOutput, MidiOutputConnection};
use std::time::{Duration, Instant};

fn connect(port: Option<&str>) -> MidiOutputConnection {
    let out = MidiOutput::new("LiveStage MTC Test").expect("CoreMIDI / ALSA");
    match port {
        Some(name) => {
            let ports = out.ports();
            let p = ports.iter().find(|p| out.port_name(p).is_ok_and(|n| n == name)).unwrap_or_else(|| panic!("no MIDI output {name}"));
            out.connect(p, "mtc").expect("connect")
        }
        #[cfg(unix)]
        None => {
            use midir::os::unix::VirtualOutput;
            out.create_virtual("LiveStage MTC Test").expect("virtual port")
        }
        #[cfg(not(unix))]
        None => panic!("pass an output port name"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rate = args.get(2).and_then(|r| FrameRate::from_token(r)).unwrap_or(FrameRate::Fps25);
    let start = Timecode::parse_with(args.first().map_or("00:59:58:00", |s| s.as_str()), rate).expect("start timecode").with_rate(rate);
    let secs: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let mut conn = connect(args.get(3).map(|s| s.as_str()));
    // Give receivers a moment to see the new source.
    std::thread::sleep(Duration::from_millis(1500));
    conn.send(&encode_full_frame(&start, 0x7F)).expect("send");
    println!("MTC {} from {start} for {secs} s", rate.label());

    let quarter = Duration::from_secs_f64(rate.frames_to_micros(0.25) / 1e6);
    let t0 = Instant::now();
    let mut label = start;
    let mut k = 0u32;
    while t0.elapsed().as_secs_f64() < secs {
        for byte in encode_quarter_frames(&label) {
            conn.send(&[0xF1, byte]).expect("send");
            k += 1;
            if let Some(wait) = (t0 + quarter * k).checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        label = label.add_frames(2);
    }
    println!("stopped at {label}");
    std::thread::sleep(Duration::from_millis(200));
}
