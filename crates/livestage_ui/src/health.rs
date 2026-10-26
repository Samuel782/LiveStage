//! Live-show hardening: fault containment, watchdogs, autosave and soak
//! telemetry.
//!
//! - Every long-running loop (show thread, output thread, UI) beats a
//!   [`Heartbeat`]. The show and output loops run each tick / frame through
//!   [`contain`], so a panic in one of them is caught, counted and reported
//!   (once per code location, see [`install_panic_hook`]) and the loop goes
//!   on with the next tick instead of taking the show down.
//! - The [`Watchdog`] thread checks those heartbeats every 10 ms and the
//!   audio device callback every 250 ms, and reports stalls, recoveries,
//!   faults and device errors to the operator log. It never waits for the
//!   runtime lock: a stalled show thread holding it delays only the log
//!   lines, which are queued.
//! - The watchdog also autosaves dirty shows (on its own writer thread), so
//!   crash recovery keeps working while the UI is frozen, and writes a health
//!   summary to the log every few minutes for long (soak) runs.

use crate::runtime::LogLevel;
use crate::show_thread::SharedRuntime;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

/// How often the watchdog checks the heartbeats.
const TICK: Duration = Duration::from_millis(10);
/// How often it looks at the runtime (audio device, autosave, log lines).
const RUNTIME_POLL: Duration = Duration::from_millis(250);
/// No audio device callback for this long means the device stopped.
const AUDIO_STALL: Duration = Duration::from_secs(1);
/// Repeated faults or device errors are reported at most this often.
const REPEAT_REPORT: Duration = Duration::from_secs(10);
/// Health summary interval in the log.
const SUMMARY_EVERY: Duration = Duration::from_secs(600);

const AUDIO_STALLED: &str = "audio device stopped responding: check the interface, then Restart audio";

// ------------------------------------------------------------- heartbeat --

/// Liveness of one loop: when it last beat, how long its worst gap was, how
/// many faults (caught panics) and stalls it had.
pub struct Heartbeat {
    pub name: &'static str,
    epoch: Instant,
    /// Last beat in ns since `epoch` (0 = never).
    last_ns: AtomicU64,
    beats: AtomicU64,
    last_gap_ns: AtomicU64,
    /// Worst gap between beats since the last health summary.
    worst_gap_ns: AtomicU64,
    faults: AtomicU64,
    stalls: AtomicU64,
    stalled: AtomicBool,
    /// Set while the loop runs; a loop that is not running is not watched.
    watched: AtomicBool,
    /// The loop ended in a panic.
    died: AtomicBool,
}

impl Heartbeat {
    pub fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            epoch: Instant::now(),
            last_ns: AtomicU64::new(0),
            beats: AtomicU64::new(0),
            last_gap_ns: AtomicU64::new(0),
            worst_gap_ns: AtomicU64::new(0),
            faults: AtomicU64::new(0),
            stalls: AtomicU64::new(0),
            stalled: AtomicBool::new(false),
            watched: AtomicBool::new(false),
            died: AtomicBool::new(false),
        })
    }

    /// Starts watching the calling loop until the guard drops; a loop that
    /// ends in a panic is reported as dead.
    pub fn enter(self: &Arc<Self>) -> Running {
        self.died.store(false, Ordering::Relaxed);
        self.last_ns.store(0, Ordering::Relaxed);
        self.beat();
        self.watched.store(true, Ordering::Release);
        Running(self.clone())
    }

    pub fn beat(&self) {
        let now = (self.epoch.elapsed().as_nanos() as u64).max(1);
        let prev = self.last_ns.swap(now, Ordering::AcqRel);
        if prev != 0 {
            let gap = now.saturating_sub(prev);
            self.last_gap_ns.store(gap, Ordering::Relaxed);
            self.worst_gap_ns.fetch_max(gap, Ordering::Relaxed);
        }
        self.beats.fetch_add(1, Ordering::Relaxed);
    }

    /// Time since the last beat; `None` before the first.
    pub fn age(&self) -> Option<Duration> {
        let last = self.last_ns.load(Ordering::Acquire);
        (last != 0).then(|| Duration::from_nanos((self.epoch.elapsed().as_nanos() as u64).saturating_sub(last)))
    }

    pub fn beats(&self) -> u64 {
        self.beats.load(Ordering::Relaxed)
    }
    pub fn faults(&self) -> u64 {
        self.faults.load(Ordering::Relaxed)
    }
    pub fn stalls(&self) -> u64 {
        self.stalls.load(Ordering::Relaxed)
    }
    pub fn is_stalled(&self) -> bool {
        self.stalled.load(Ordering::Relaxed)
    }
    pub fn take_worst_gap(&self) -> Duration {
        Duration::from_nanos(self.worst_gap_ns.swap(0, Ordering::Relaxed))
    }
}

/// Keeps a heartbeat watched while a loop runs (see [`Heartbeat::enter`]).
pub struct Running(Arc<Heartbeat>);

impl Drop for Running {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.died.store(true, Ordering::Relaxed);
        }
        self.0.watched.store(false, Ordering::Release);
    }
}

/// Runs one tick or frame of a loop, catching a panic so the loop survives
/// it. The fault is counted on `hb`; the panic hook has already reported it.
pub fn contain<R>(hb: &Heartbeat, f: impl FnOnce() -> R) -> Option<R> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(_) => {
            hb.faults.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

// --------------------------------------------------------------- checks --

/// What the watchdog noticed about a heartbeat.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Stalled(Duration),
    Recovered(Duration),
    Faulted(u64),
    Died,
}

/// A heartbeat under watch and what the watchdog last reported about it.
pub struct Watched {
    pub hb: Arc<Heartbeat>,
    stall_after: Duration,
    /// Report to the operator log (the UI heartbeat only goes to the file).
    operator: bool,
    faults_seen: u64,
    faults_reported_at: Option<Instant>,
    died_reported: bool,
    summary_beats: u64,
}

impl Watched {
    pub fn new(hb: Arc<Heartbeat>, stall_after: Duration, operator: bool) -> Self {
        Self { hb, stall_after, operator, faults_seen: 0, faults_reported_at: None, died_reported: false, summary_beats: 0 }
    }

    pub fn check(&mut self, now: Instant) -> Vec<Event> {
        let hb = &self.hb;
        let mut events = Vec::new();
        if hb.died.load(Ordering::Relaxed) && !self.died_reported {
            self.died_reported = true;
            events.push(Event::Died);
        }
        let faults = hb.faults();
        if faults > self.faults_seen && self.faults_reported_at.is_none_or(|t| now.duration_since(t) >= REPEAT_REPORT) {
            self.faults_seen = faults;
            self.faults_reported_at = Some(now);
            events.push(Event::Faulted(faults));
        }
        if !hb.watched.load(Ordering::Acquire) {
            hb.stalled.store(false, Ordering::Relaxed);
            return events;
        }
        let Some(age) = hb.age() else { return events };
        let stalled = hb.is_stalled();
        if !stalled && age >= self.stall_after {
            hb.stalled.store(true, Ordering::Relaxed);
            hb.stalls.fetch_add(1, Ordering::Relaxed);
            events.push(Event::Stalled(age));
        } else if stalled && age < self.stall_after {
            hb.stalled.store(false, Ordering::Relaxed);
            events.push(Event::Recovered(Duration::from_nanos(hb.last_gap_ns.load(Ordering::Relaxed))));
        }
        events
    }

    /// Log line for an event: level and text.
    fn describe(&self, e: &Event) -> (LogLevel, String) {
        let name = self.hb.name;
        let ms = |d: &Duration| d.as_secs_f64() * 1e3;
        match e {
            Event::Stalled(d) => (LogLevel::Error, format!("watchdog: {name} thread stalled (no progress for {:.0} ms)", ms(d))),
            Event::Recovered(d) => (LogLevel::Warn, format!("watchdog: {name} thread recovered after {:.0} ms", ms(d))),
            Event::Faulted(n) => (LogLevel::Error, format!("watchdog: {name} thread caught a fault and kept running ({n} so far, see the log)")),
            Event::Died => (LogLevel::Error, format!("watchdog: {name} thread exited after a fault; save and restart LiveStage")),
        }
    }
}

// ------------------------------------------------------------- watchdog --

/// Watchdog results the UI shows, and the autosave interval it reads.
#[derive(Default)]
pub struct WatchState {
    /// Autosave interval (s, 0 = off), set by the UI from the preferences.
    pub autosave_secs: AtomicU64,
    pub autosaves: AtomicU64,
    /// Consecutive failed autosaves.
    pub autosave_failures: AtomicU64,
    /// Unix seconds of the last autosave written (0 = none yet).
    pub autosaved_at: AtomicU64,
    pub audio_stalled: AtomicBool,
}

/// A snapshot of a dirty show for the autosave writer.
struct AutosaveJob {
    root: PathBuf,
    generation: u64,
    project: livestage_core::Project,
    taken: SystemTime,
}

type AutosaveDone = (PathBuf, u64, Result<(), String>);

/// Writes one autosave. If the show was saved for real while the copy was
/// being written, the copy is stale and is removed again, so it can never
/// prompt a recovery of older edits.
fn write_autosave(job: &AutosaveJob) -> Result<(), String> {
    livestage_core::bundle::write_autosave(&job.root, &job.project).map_err(|e| e.to_string())?;
    let manifest = job.root.join(livestage_core::bundle::MANIFEST_FILE);
    if std::fs::metadata(manifest).and_then(|m| m.modified()).is_ok_and(|m| m >= job.taken) {
        let _ = std::fs::remove_file(job.root.join(livestage_core::bundle::AUTOSAVE_FILE));
    }
    Ok(())
}

#[derive(Default)]
struct AudioWatch {
    stalled: bool,
    errors_seen: u64,
    errors_reported_at: Option<Instant>,
    underruns: u64,
    device_errors: u64,
}

pub struct Watchdog {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub state: Arc<WatchState>,
}

impl Watchdog {
    pub fn spawn(rt: SharedRuntime, mut watched: Vec<Watched>, state: Arc<WatchState>) -> std::io::Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let (save_tx, save_rx) = crossbeam_channel::bounded::<AutosaveJob>(1);
        let (done_tx, done_rx) = crossbeam_channel::unbounded::<AutosaveDone>();
        let writer = std::thread::Builder::new().name("livestage-autosave".into()).spawn(move || {
            for job in save_rx {
                let result = write_autosave(&job);
                let _ = done_tx.send((job.root, job.generation, result));
            }
        })?;
        let (r, st) = (running.clone(), state.clone());
        let handle = std::thread::Builder::new().name("livestage-watchdog".into()).spawn(move || {
            let started = Instant::now();
            let mut pending: Vec<(LogLevel, String)> = Vec::new();
            let mut audio = AudioWatch::default();
            let (mut next_poll, mut last_autosave) = (started, started);
            let (mut next_summary, mut last_summary) = (started + SUMMARY_EVERY, started);
            // (bundle root, edit generation) of the autosave on disk.
            let mut autosaved: Option<(PathBuf, u64)> = None;
            let mut in_flight = false;
            while r.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                let now = Instant::now();
                for w in &mut watched {
                    for e in w.check(now) {
                        let (level, text) = w.describe(&e);
                        match level {
                            LogLevel::Error => tracing::error!("{text}"),
                            _ => tracing::warn!("{text}"),
                        }
                        if w.operator {
                            pending.push((level, text));
                        }
                    }
                }
                for (root, generation, result) in done_rx.try_iter() {
                    in_flight = false;
                    match result {
                        Ok(()) => {
                            tracing::debug!("autosaved");
                            autosaved = Some((root, generation));
                            st.autosaves.fetch_add(1, Ordering::Relaxed);
                            let unix = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                            st.autosaved_at.store(unix, Ordering::Relaxed);
                            if st.autosave_failures.swap(0, Ordering::Relaxed) > 0 {
                                pending.push((LogLevel::Info, "autosave working again".into()));
                            }
                        }
                        Err(e) => {
                            tracing::warn!("autosave failed: {e}");
                            if st.autosave_failures.fetch_add(1, Ordering::Relaxed) == 0 {
                                pending.push((LogLevel::Warn, format!("autosave failed: {e} (retrying)")));
                            }
                        }
                    }
                }
                if now >= next_summary {
                    tracing::info!("{}", summary(started.elapsed(), now - last_summary, &mut watched, &audio, &st));
                    (next_summary, last_summary) = (now + SUMMARY_EVERY, now);
                }
                if now < next_poll {
                    continue;
                }
                // A stalled show thread may hold the lock: try again next tick.
                let Some(mut rt) = rt.try_lock_for(Duration::from_millis(2)) else { continue };
                next_poll = now + RUNTIME_POLL;
                for (level, text) in pending.drain(..) {
                    rt.note(level, text);
                }
                check_audio(&mut rt, &mut audio, &st, now);
                let every = st.autosave_secs.load(Ordering::Relaxed);
                if every > 0 && !in_flight && last_autosave.elapsed() >= Duration::from_secs(every) {
                    last_autosave = now;
                    let key = (rt.bundle.root().to_path_buf(), rt.edit_generation);
                    if rt.dirty && autosaved.as_ref() != Some(&key) {
                        let job =
                            AutosaveJob { root: key.0, generation: key.1, project: rt.bundle.project.clone(), taken: SystemTime::now() };
                        in_flight = save_tx.try_send(job).is_ok();
                    }
                }
            }
            tracing::info!("{}", summary(started.elapsed(), last_summary.elapsed(), &mut watched, &audio, &st));
            drop(save_tx);
            let _ = writer.join(); // lets an autosave in progress finish
        })?;
        Ok(Self { running, handle: Some(handle), state })
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Audio device health: callbacks still arriving, errors reported.
fn check_audio(rt: &mut crate::runtime::ShowRuntime, w: &mut AudioWatch, st: &WatchState, now: Instant) {
    let Some(audio) = rt.audio.as_ref() else {
        w.stalled = false;
        st.audio_stalled.store(false, Ordering::Relaxed);
        return;
    };
    let (errors, underruns) = (audio.device_errors(), audio.underruns());
    let stalled = audio.callback_age().is_some_and(|age| age >= AUDIO_STALL);
    (w.underruns, w.device_errors) = (underruns, errors);
    if errors < w.errors_seen {
        w.errors_seen = errors; // the engine was restarted
    }
    if errors > w.errors_seen && w.errors_reported_at.is_none_or(|t| now.duration_since(t) >= REPEAT_REPORT) {
        w.errors_seen = errors;
        w.errors_reported_at = Some(now);
        rt.log(LogLevel::Error, format!("audio device reported an error ({errors} so far)"));
    }
    if stalled != w.stalled {
        w.stalled = stalled;
        st.audio_stalled.store(stalled, Ordering::Relaxed);
        if stalled {
            rt.audio_error = Some(AUDIO_STALLED.into());
            rt.log(LogLevel::Error, AUDIO_STALLED);
        } else {
            if rt.audio_error.as_deref() == Some(AUDIO_STALLED) {
                rt.audio_error = None;
            }
            rt.log(LogLevel::Info, "audio device responding again");
        }
    }
}

/// One line for the log: rates, worst gaps, stalls and faults per loop,
/// audio, autosave and process resources since the last summary.
fn summary(up: Duration, window: Duration, watched: &mut [Watched], audio: &AudioWatch, st: &WatchState) -> String {
    let secs = up.as_secs();
    let mut out = format!("health: up {}h{:02}m{:02}s", secs / 3600, secs / 60 % 60, secs % 60);
    for w in watched.iter_mut() {
        let beats = w.hb.beats();
        let rate = (beats - w.summary_beats.min(beats)) as f64 / window.as_secs_f64().max(1e-3);
        w.summary_beats = beats;
        out += &format!(
            " | {} {rate:.0}/s, worst gap {:.1} ms, {} stalls, {} faults",
            w.hb.name,
            w.hb.take_worst_gap().as_secs_f64() * 1e3,
            w.hb.stalls(),
            w.hb.faults()
        );
    }
    out += &format!(" | audio {} underruns, {} device errors", audio.underruns, audio.device_errors);
    out += &format!(
        " | autosaves {}, failing {}",
        st.autosaves.load(Ordering::Relaxed),
        st.autosave_failures.load(Ordering::Relaxed)
    );
    let p = proc_stats();
    let opt = |v: Option<u64>| v.map_or("?".to_string(), |v| v.to_string());
    out += &format!(" | rss {} MB, {} threads, {} fds", opt(p.rss_bytes.map(|b| b >> 20)), opt(p.threads), opt(p.fds));
    out
}

// --------------------------------------------------------------- panics --

/// Panics seen per code location (bounded by the number of panic sites).
static PANICS: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Default::default);

/// Reports panics to the log, and the first one from each code location with
/// a backtrace to `crash.log` in `crash_dir`. A panic that repeats (a fault
/// hit on every tick) is logged again only at the 2nd, 4th, 8th, ...
/// occurrence, so it cannot flood the log over a long show.
pub fn install_panic_hook(crash_dir: Option<PathBuf>) {
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map_or_else(|| "unknown location".into(), |l| format!("{}:{}", l.file(), l.line()));
        let count = {
            let mut seen = PANICS.lock();
            let n = seen.entry(location.clone()).or_insert(0);
            *n += 1;
            *n
        };
        if !count.is_power_of_two() {
            return;
        }
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(non-text panic payload)".into());
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        if count > 1 {
            tracing::error!(thread, location, count, "panic repeated: {msg}");
            return;
        }
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(thread, location, "panic: {msg}\n{backtrace}");
        if let Some(dir) = &crash_dir {
            let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let report = format!(
                "=== {when} · LiveStage {} · pid {} ===\nthread '{thread}' panicked at {location}:\n{msg}\n{backtrace}\n",
                env!("CARGO_PKG_VERSION"),
                std::process::id()
            );
            crate::logging::append_crash_report(dir, &report);
        }
    }));
}

// -------------------------------------------------------------- signals --

static STOP: AtomicBool = AtomicBool::new(false);

/// Ctrl+C / SIGTERM (a terminal, `kill`, a soak script) ask for the same
/// clean quit as closing the window, instead of killing the process with the
/// show unsaved and its session marker left behind.
pub fn install_stop_signals() {
    #[cfg(unix)]
    {
        extern "C" fn on_signal(_: libc::c_int) {
            STOP.store(true, Ordering::Relaxed); // async-signal-safe
        }
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
        }
    }
}

/// Set once a stop signal has arrived.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

// ------------------------------------------------------------- process --

/// Resident memory, threads and open file descriptors of this process.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcStats {
    pub rss_bytes: Option<u64>,
    pub threads: Option<u64>,
    pub fds: Option<u64>,
}

pub fn proc_stats() -> ProcStats {
    let mut s = ProcStats::default();
    #[cfg(target_os = "macos")]
    {
        // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let got = unsafe {
            libc::proc_pidinfo(std::process::id() as i32, libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size)
        };
        if got == size {
            s.rss_bytes = Some(info.pti_resident_size);
            s.threads = Some(info.pti_threadnum as u64);
        }
    }
    #[cfg(target_os = "linux")]
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        let field = |name: &str| {
            status.lines().find_map(|l| l.strip_prefix(name)).and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        };
        s.rss_bytes = field("VmRSS:").map(|kb| kb * 1024);
        s.threads = field("Threads:");
    }
    #[cfg(unix)]
    {
        // Minus the descriptor read_dir itself holds open.
        s.fds = std::fs::read_dir("/dev/fd").ok().map(|d| d.count().saturating_sub(1) as u64);
    }
    s
}

/// True when a process with this id is running (another LiveStage holding
/// a show's session marker).
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else { return false };
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contain_keeps_a_loop_running_through_a_panic() {
        let hb = Heartbeat::new("test");
        let mut ticks = 0;
        for i in 0..5 {
            contain(&hb, || {
                if i == 2 {
                    panic!("injected fault");
                }
                ticks += 1;
            });
        }
        assert_eq!((ticks, hb.faults()), (4, 1));
    }

    #[test]
    fn stall_and_recovery_are_reported_once() {
        let hb = Heartbeat::new("show");
        let _running = hb.enter();
        let mut w = Watched::new(hb.clone(), Duration::from_millis(30), true);
        assert!(w.check(Instant::now()).is_empty());
        std::thread::sleep(Duration::from_millis(60));
        let events = w.check(Instant::now());
        assert!(matches!(events[..], [Event::Stalled(d)] if d >= Duration::from_millis(30)), "{events:?}");
        assert!(w.check(Instant::now()).is_empty(), "a stall is reported once");
        assert!(hb.is_stalled());
        hb.beat();
        let events = w.check(Instant::now());
        assert!(matches!(events[..], [Event::Recovered(d)] if d >= Duration::from_millis(60)), "{events:?}");
        assert_eq!(hb.stalls(), 1);
        assert!(!hb.is_stalled());
    }

    #[test]
    fn stopped_loops_are_not_stalls_but_panicked_ones_are_dead() {
        let hb = Heartbeat::new("output");
        let mut w = Watched::new(hb.clone(), Duration::from_millis(5), true);
        drop(hb.enter()); // a loop that stopped on purpose
        std::thread::sleep(Duration::from_millis(20));
        assert!(w.check(Instant::now()).is_empty());

        let h = hb.clone();
        let _ = std::thread::spawn(move || {
            let _running = h.enter();
            panic!("loop body outside contain");
        })
        .join();
        assert_eq!(w.check(Instant::now()), [Event::Died]);
        assert!(w.check(Instant::now()).is_empty());
    }

    #[test]
    fn repeated_faults_are_rate_limited() {
        let hb = Heartbeat::new("show");
        let mut w = Watched::new(hb.clone(), Duration::from_secs(1), true);
        let now = Instant::now();
        for _ in 0..1000 {
            contain(&hb, || panic!("every tick"));
        }
        assert_eq!(w.check(now), [Event::Faulted(1000)]);
        contain(&hb, || panic!("again"));
        assert!(w.check(now + Duration::from_secs(1)).is_empty());
        assert_eq!(w.check(now + REPEAT_REPORT), [Event::Faulted(1001)]);
    }

    #[test]
    fn stale_autosave_is_removed_after_a_real_save() {
        let dir = std::env::temp_dir().join(format!("livestage-autosave-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = livestage_core::ProjectBundle::create(&dir, "Race").unwrap();
        let root = bundle.root().to_path_buf();
        let job = |taken| AutosaveJob { root: root.clone(), generation: 1, project: bundle.project.clone(), taken };
        // Snapshot taken before the manifest was last written: stale.
        write_autosave(&job(SystemTime::UNIX_EPOCH)).unwrap();
        assert!(bundle.recovery_available().is_none());
        // Snapshot newer than the manifest: kept for recovery.
        std::thread::sleep(Duration::from_millis(20));
        write_autosave(&job(SystemTime::now())).unwrap();
        assert!(bundle.recovery_available().is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Soak run: the real runtime, show thread and watchdog for
    /// `LIVESTAGE_SOAK_SECS` (default 30 s), firing a GO and an edit five
    /// times a second on the demo show without its audio cues. Checks that
    /// a deliberate 300 ms lock hold is reported as a stall and recovery,
    /// that autosave keeps up, and that threads, descriptors, memory and
    /// the operator log stay bounded. Opens the default audio device
    /// (silently), so it is ignored by default:
    /// `LIVESTAGE_SOAK_SECS=28800 cargo test -p livestage_ui --release soak -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn soak() {
        use crate::show_thread::{ShowThread, SHOW_RATE_HZ};
        use livestage_control::{ControlSource, ShowCommand};
        use livestage_core::cue::{AnyCue, Cue as _};

        let secs: u64 = std::env::var("LIVESTAGE_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
        let dir = std::env::temp_dir().join(format!("livestage-soak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut bundle = livestage_core::ProjectBundle::create(&dir, "Soak").unwrap();
        let mut p: livestage_core::Project =
            serde_json::from_slice(&std::fs::read("../../shows/Demo Show.livestage/project.json").unwrap()).unwrap();
        p.dmx = Default::default(); // no Art-Net / sACN traffic from a test
        p.control.osc_enabled = false;
        p.control.websocket_enabled = false;
        for list in &mut p.cue_lists {
            list.cues.retain(|c| !matches!(c, AnyCue::Audio(_) | AnyCue::Timeline(_)));
        }
        p.carts.clear();
        bundle.project = p;
        let root = bundle.root().to_path_buf();
        let shared: SharedRuntime = Arc::new(Mutex::new(crate::runtime::ShowRuntime::new(bundle)));
        shared.lock().reset_lists();
        let video = shared.lock().video_receiver();
        let show = ShowThread::spawn(shared.clone(), SHOW_RATE_HZ).unwrap();
        let state = Arc::new(WatchState::default());
        state.autosave_secs.store(1, Ordering::Relaxed);
        let watched = vec![Watched::new(show.heartbeat.clone(), Duration::from_millis(100), true)];
        let dog = Watchdog::spawn(shared.clone(), watched, state.clone()).unwrap();

        let start = Instant::now();
        let (mut baseline, mut worst) = (None::<ProcStats>, ProcStats::default());
        let (mut gos, mut held, mut last_sample) = (0u64, false, start);
        while start.elapsed() < Duration::from_secs(secs) {
            std::thread::sleep(Duration::from_millis(200));
            {
                let mut rt = shared.lock();
                rt.command(ControlSource::Ui, ShowCommand::Go { list: None });
                if let Some(list) = rt.bundle.project.cue_lists.first_mut() {
                    if let Some(c) = list.cues.first_mut() {
                        c.header_mut().notes = format!("soak edit {gos}");
                    }
                }
                rt.mark_changed();
            }
            gos += 1;
            video.try_iter().for_each(drop);
            if !held && start.elapsed() >= Duration::from_secs(secs / 2) {
                held = true;
                let _guard = shared.lock();
                std::thread::sleep(Duration::from_millis(300));
            }
            if last_sample.elapsed() >= Duration::from_secs(5) {
                last_sample = Instant::now();
                let s = proc_stats();
                println!("{:>6.0} s: rss {:?} MB, threads {:?}, fds {:?}", start.elapsed().as_secs_f64(), s.rss_bytes.map(|b| b >> 20), s.threads, s.fds);
                match baseline {
                    // After warm-up (engines started, caches filled).
                    None if start.elapsed() >= Duration::from_secs(10) => baseline = Some(s),
                    _ => {}
                }
                worst.rss_bytes = worst.rss_bytes.max(s.rss_bytes);
                worst.threads = worst.threads.max(s.threads);
                worst.fds = worst.fds.max(s.fds);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        let log: Vec<String> = shared.lock().log.iter().map(|l| l.text.clone()).collect();
        println!("{gos} GOs; {} autosaves; show stalls {}, faults {}", state.autosaves.load(Ordering::Relaxed), show.heartbeat.stalls(), show.heartbeat.faults());
        drop(dog);
        drop(show);

        assert!(log.len() <= 500, "operator log is capped");
        assert!(log.iter().any(|l| l.contains("show thread stalled")), "the held lock was reported");
        assert!(log.iter().any(|l| l.contains("show thread recovered")), "and its recovery");
        assert_eq!(show_faults(&log), 0);
        assert!(state.autosaves.load(Ordering::Relaxed) >= secs / 3, "autosave kept up");
        assert!(root.join(livestage_core::bundle::AUTOSAVE_FILE).is_file());
        if let (Some(b), true) = (baseline, cfg!(any(target_os = "macos", target_os = "linux"))) {
            assert!(worst.threads <= b.threads.map(|t| t + 2), "threads grew: {b:?} -> {worst:?}");
            assert!(worst.fds <= b.fds.map(|f| f + 8), "descriptors grew: {b:?} -> {worst:?}");
            assert!(worst.rss_bytes <= b.rss_bytes.map(|r| r + (64 << 20)), "memory grew: {b:?} -> {worst:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn show_faults(log: &[String]) -> usize {
        log.iter().filter(|l| l.contains("caught a fault")).count()
    }

    #[test]
    fn process_stats_and_liveness() {
        let s = proc_stats();
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(s.rss_bytes.is_some_and(|b| b > 1 << 20), "{s:?}");
            assert!(s.threads.is_some_and(|t| t >= 1), "{s:?}");
            assert!(s.fds.is_some_and(|f| f >= 3), "{s:?}");
            assert!(process_alive(std::process::id()));
            assert!(!process_alive(u32::MAX - 1));
        }
    }
}
