//! The show thread: ticks every cue list, executes commands and drives the
//! audio / DMX / control engines at a fixed high rate, independent of the UI.
//!
//! The UI only holds the runtime lock while laying out panels (no GPU work,
//! no presentation), so a stalled or vsync-blocked UI never delays a cue.
//! macOS window drags or a slow frame no longer freeze the show either.

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
}

impl ShowThread {
    pub fn spawn(rt: SharedRuntime, rate_hz: u32) -> std::io::Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let stats: Arc<ShowStats> = rt.lock().show_stats.clone();
        let r = running.clone();
        let period = Duration::from_secs_f64(1.0 / rate_hz.max(1) as f64);
        let handle = std::thread::Builder::new().name("livestage-show".into()).spawn(move || {
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
                    guard.tick();
                    ShowStats::record(&stats.max_tick_us, locked.elapsed().as_micros() as u64);
                }
                stats.ticks.fetch_add(1, Ordering::Relaxed);
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
        Ok(Self { running, handle: Some(handle) })
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
}
