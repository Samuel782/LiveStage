//! The show thread: ticks every cue list, executes commands and drives the
//! audio / DMX / control engines at a fixed high rate, independent of the UI.
//!
//! The UI never holds the runtime lock while laying out panels: it copies
//! what it shows, lays out against the copy and folds edits back
//! ([`crate::view`]), so a slow, stalled or vsync-blocked UI frame never
//! delays a cue. macOS window drags no longer freeze the show either.
//!
//! Each tick beats the thread's [`Heartbeat`] for the watchdog, and a panic
//! inside a tick is contained: that tick is lost, the show keeps running.

use crate::health::{contain, Heartbeat};
use crate::runtime::{ShowRuntime, ShowStats};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub type SharedRuntime = Arc<Mutex<ShowRuntime>>;

/// Default show-control rate. 1 kHz gives ≤1 ms cue resolution for DMX, OSC
/// and MIDI triggers. Audio is sample-accurate regardless.
pub const SHOW_RATE_HZ: u32 = 1_000;

pub struct ShowThread {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub heartbeat: Arc<Heartbeat>,
}

impl ShowThread {
    pub fn spawn(rt: SharedRuntime, rate_hz: u32) -> std::io::Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let stats: Arc<ShowStats> = rt.lock().show_stats.clone();
        let r = running.clone();
        let heartbeat = Heartbeat::new("show");
        let hb = heartbeat.clone();
        let period = Duration::from_secs_f64(1.0 / rate_hz.max(1) as f64);
        let handle = std::thread::Builder::new().name("livestage-show".into()).spawn(move || {
            let _running = hb.enter();
            let class = crate::priority::make_current_thread_realtime(period);
            tracing::info!(scheduling = %class, rate_hz, "show thread started");
            *stats.scheduling.lock() = class;
            let mut next = Instant::now();
            while r.load(Ordering::Relaxed) {
                let start = Instant::now();
                ShowStats::record(&stats.max_late_us, start.saturating_duration_since(next).as_micros() as u64);
                {
                    let mut guard = rt.lock();
                    let locked = Instant::now();
                    ShowStats::record(&stats.max_wait_us, (locked - start).as_micros() as u64);
                    contain(&hb, || guard.tick());
                    ShowStats::record(&stats.max_tick_us, locked.elapsed().as_micros() as u64);
                }
                stats.ticks.fetch_add(1, Ordering::Relaxed);
                hb.beat();
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else if now - next > period * 50 {
                    // Fell far behind (e.g. machine slept): resynchronise instead of bursting.
                    next = now;
                }
            }
        })?;
        Ok(Self { running, handle: Some(handle), heartbeat })
    }
}

impl Drop for ShowThread {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<crate::runtime::ShowRuntime>();
    }

    use std::time::Duration;

    /// Worst show-thread lock wait and lateness (µs) while a UI with a
    /// `layout`-long frame runs `frames` frames, plus the median / max time
    /// the UI held the lock per frame.
    struct Run {
        wait_us: u64,
        late_us: u64,
        holds: Vec<Duration>,
    }

    /// Runs the show thread at 1 kHz on a show with `cues` cues and drives
    /// UI frames against it, either the old way (`under_lock`: layout while
    /// holding the runtime) or with the frame copy, editing a cue every frame.
    fn run_ui(cues: usize, layout: Duration, under_lock: bool) -> Run {
        use super::*;
        use crate::runtime::{renew_ids, ShowStats};
        use crate::view::UiRuntime;
        use livestage_core::cue::Cue;
        use livestage_core::ProjectBundle;

        let dir = std::env::temp_dir().join(format!("livestage-isolation-{}-{cues}-{under_lock}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut bundle = ProjectBundle::create(&dir, "Isolation").unwrap();
        let demo: livestage_core::Project =
            serde_json::from_slice(&std::fs::read("../../shows/Demo Show.livestage/project.json").unwrap()).unwrap();
        let mut p = demo.clone();
        p.dmx = Default::default(); // no Art-Net / sACN traffic from a test
        p.control.osc_enabled = false;
        p.control.websocket_enabled = false;
        let template = demo.cue_lists[0].cues.clone();
        let list = &mut p.cue_lists[0];
        list.cues.truncate(cues);
        while list.cues.len() < cues {
            let mut c = template[list.cues.len() % template.len()].clone();
            renew_ids(&mut c);
            c.header_mut().number = (list.cues.len() + 1).to_string();
            list.cues.push(c);
        }
        bundle.project = p;
        let shared: SharedRuntime = Arc::new(Mutex::new(crate::runtime::ShowRuntime::new(bundle)));
        shared.lock().reset_lists();
        let stats = shared.lock().show_stats.clone();
        let show = ShowThread::spawn(shared.clone(), SHOW_RATE_HZ).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        ShowStats::take(&stats.max_wait_us);
        ShowStats::take(&stats.max_late_us);

        let mut holds = Vec::new();
        let mut view = UiRuntime::new(&shared.lock());
        let edited = cues / 2;
        for i in 0..30 {
            if under_lock {
                let t = Instant::now();
                let guard = shared.lock();
                std::thread::sleep(layout);
                drop(guard);
                holds.push(t.elapsed());
            } else {
                let t = Instant::now();
                view.sync(&shared.lock());
                let mut held = t.elapsed();
                view.begin_frame();
                view.bundle.project.cue_lists[0].cues[edited].header_mut().name = format!("edit {i}");
                view.mark_changed();
                std::thread::sleep(layout);
                let edits = view.take_edits();
                let t = Instant::now();
                view.apply(&mut shared.lock(), edits);
                held += t.elapsed();
                holds.push(held);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let run = Run { wait_us: ShowStats::take(&stats.max_wait_us), late_us: ShowStats::take(&stats.max_late_us), holds };
        if !under_lock {
            let name = shared.lock().bundle.project.cue_lists[0].cues[edited].header().name.clone();
            assert_eq!(name, "edit 29", "edits reach the live project");
        }
        drop(show);
        let _ = std::fs::remove_dir_all(&dir);
        run
    }

    /// A deliberately slow UI frame must not hold up the 1 kHz show thread:
    /// the show thread's worst lock wait has to stay small and must not grow
    /// with the layout time. The old pattern (layout under the lock) runs as
    /// a control. Opens the default audio device, so it is ignored by default:
    /// `cargo test -p livestage_ui --release slow_ui -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn slow_ui_does_not_delay_show_thread() {
        let report = |what: &str, r: &mut Run| {
            r.holds.sort();
            println!(
                "{what:<34} show thread waited ≤ {:>6} µs, ticked ≤ {:>6} µs late; UI held the lock median {:?}, max {:?}",
                r.wait_us,
                r.late_us,
                r.holds[r.holds.len() / 2],
                r.holds[r.holds.len() - 1]
            );
        };
        let mut old = run_ui(1000, Duration::from_millis(20), true);
        report("layout under lock, 1000 cues, 20 ms", &mut old);
        let mut demo = run_ui(7, Duration::from_millis(20), false);
        report("frame copy, demo show, 20 ms", &mut demo);
        let mut big = run_ui(1000, Duration::from_millis(20), false);
        report("frame copy, 1000 cues, 20 ms", &mut big);
        let mut slower = run_ui(1000, Duration::from_millis(80), false);
        report("frame copy, 1000 cues, 80 ms", &mut slower);

        assert!(old.wait_us >= 15_000, "control run should block ({} µs)", old.wait_us);
        assert!(demo.wait_us < 1_500, "demo show: show thread waited {} µs", demo.wait_us);
        assert!(big.wait_us < 5_000, "1000 cues: show thread waited {} µs", big.wait_us);
        assert!(slower.wait_us < 5_000, "a 4× slower layout must not lengthen the wait ({} µs)", slower.wait_us);
    }
}
