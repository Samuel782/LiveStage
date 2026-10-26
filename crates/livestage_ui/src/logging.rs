//! Log files for long runs: every event goes to stdout and to a size-capped,
//! rotating file (`livestage.log`, `livestage.log.1`, ...), so an 8-hour show
//! or soak test leaves a complete record without filling the disk.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

pub const LOG_FILE: &str = "livestage.log";
/// Size at which the log rotates.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Rotated files kept besides the current one (≤ 96 MB in all).
const KEEP: usize = 5;

/// Where LiveStage writes its logs and crash reports.
pub fn log_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Logs/LiveStage"))
    } else {
        crate::prefs::config_dir().map(|d| d.join("logs"))
    }
}

/// An append-only file that rotates at `max_bytes`. Writes never fail: a log
/// that cannot be written (disk full, folder removed) is skipped, never
/// allowed to take the show down with it.
pub struct RollingFile {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    written: u64,
}

impl RollingFile {
    pub fn open(path: PathBuf, max_bytes: u64, keep: usize) -> Self {
        let mut f = Self { path, max_bytes, keep, file: None, written: 0 };
        f.reopen();
        f
    }

    fn reopen(&mut self) {
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.written = self.file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
    }

    /// `log` → `log.1` → ... → `log.<keep>` (the oldest is dropped).
    fn rotate(&mut self) {
        self.file = None;
        let numbered = |n: usize| PathBuf::from(format!("{}.{n}", self.path.display()));
        let _ = fs::remove_file(numbered(self.keep));
        for n in (1..self.keep).rev() {
            let _ = fs::rename(numbered(n), numbered(n + 1));
        }
        let _ = fs::rename(&self.path, numbered(1));
        self.reopen();
    }
}

impl Write for RollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written + buf.len() as u64 > self.max_bytes {
            self.rotate();
        }
        if self.file.is_none() {
            self.reopen();
        }
        if let Some(f) = &mut self.file {
            if f.write_all(buf).is_ok() {
                self.written += buf.len() as u64;
            } else {
                self.file = None; // retried on the next event
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Installs the global subscriber: stdout plus the rotating file in
/// [`log_dir`]. Returns the log file's path when the file could be opened.
pub fn init() -> Option<PathBuf> {
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into())
    };
    let path = log_dir().filter(|d| fs::create_dir_all(d).is_ok()).map(|d| d.join(LOG_FILE));
    let file = path.as_ref().map(|p| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_thread_names(true)
            .with_writer(std::sync::Mutex::new(RollingFile::open(p.clone(), MAX_BYTES, KEEP)))
            .with_filter(filter())
    });
    use tracing_subscriber::Layer as _;
    tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().with_filter(filter())).with(file).init();
    path
}

/// Appends `text` to `crash.log` next to the logs, rotating it at 1 MB.
pub fn append_crash_report(dir: &Path, text: &str) {
    let path = dir.join("crash.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > 1024 * 1024) {
        let _ = fs::rename(&path, dir.join("crash.log.1"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(text.as_bytes());
        let _ = f.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_caps_the_number_of_files() {
        let dir = std::env::temp_dir().join(format!("livestage-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        let mut log = RollingFile::open(path.clone(), 1000, 3);
        let line = [b'x'; 99];
        for _ in 0..100 {
            log.write_all(&line).unwrap();
            log.write_all(b"\n").unwrap();
        }
        let mut names: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["livestage.log", "livestage.log.1", "livestage.log.2", "livestage.log.3"]);
        for n in &names {
            assert!(fs::metadata(dir.join(n)).unwrap().len() <= 1000, "{n} is over the cap");
        }
        // A missing folder is not an error: the write is skipped.
        fs::remove_dir_all(&dir).unwrap();
        log.write_all(b"lost\n").unwrap();
    }
}
