//! Lists NDI sources, or receives one and reports a frame.
//!
//! ```text
//! cargo run -p livestage_video --example ndi_probe            # list sources
//! cargo run -p livestage_video --example ndi_probe -- "NAME"  # grab a frame (substring match)
//! ```

use livestage_core::ShowTime;
use livestage_video::ndi;
use livestage_video::source::VideoSource;
use std::time::{Duration, Instant};

fn main() {
    let Some(lib) = ndi::runtime() else {
        eprintln!("NDI runtime not installed");
        std::process::exit(2);
    };
    println!("NDI {} ({})", lib.version, lib.path.display());
    let want = std::env::args().nth(1);
    let deadline = Instant::now() + Duration::from_secs(10);
    let source = loop {
        let sources = ndi::discovered_sources();
        if want.is_none() && !sources.is_empty() && Instant::now() + Duration::from_secs(7) > deadline {
            for s in &sources {
                println!("  {}  {}", s.name, s.url.as_deref().unwrap_or(""));
            }
            return;
        }
        if let Some(w) = &want {
            if let Some(s) = sources.iter().find(|s| s.name.contains(w.as_str())) {
                break s.name.clone();
            }
        }
        if Instant::now() > deadline {
            eprintln!("no matching source (found {})", sources.len());
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let mut rx = ndi::NdiVideoSource::connect(&source).expect("receiver");
    loop {
        if let Some(f) = rx.frame_at(ShowTime::ZERO) {
            let c = &f.data[((f.height / 2 * f.width + f.width / 2) * 4) as usize..][..4];
            println!("{source}: {}x{} centre RGBA {c:?}", f.width, f.height);
            return;
        }
        if Instant::now() > deadline {
            eprintln!("connected to {source} but no frame arrived");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
