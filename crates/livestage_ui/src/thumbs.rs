//! Thumbnail cache for the operator UI (next-cue previews, slide strips,
//! multiview cells). Decoding runs on worker threads; the UI thread only
//! uploads finished thumbnails as egui textures.

use livestage_core::ShowTime;
use livestage_video::source::BackendRegistry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Thumbnails fit in this box (enough for a 2× multiview cell).
const MAX: (u32, u32) = (640, 360);
const WORKERS: usize = 2;
const CAPACITY: usize = 400;

type Key = (PathBuf, u64);

enum Entry {
    Pending,
    Ready(egui::TextureHandle),
    Failed,
}

struct Workers {
    jobs: crossbeam_channel::Sender<Key>,
    done: crossbeam_channel::Receiver<(Key, Option<egui::ColorImage>)>,
}

#[derive(Default)]
pub struct Thumbs {
    map: HashMap<Key, Entry>,
    workers: Option<Workers>,
}

impl Thumbs {
    fn workers(&mut self) -> &Workers {
        self.workers.get_or_insert_with(|| {
            let (jobs, job_rx) = crossbeam_channel::unbounded::<Key>();
            let (done_tx, done) = crossbeam_channel::unbounded();
            for n in 0..WORKERS {
                let (rx, tx) = (job_rx.clone(), done_tx.clone());
                let _ = std::thread::Builder::new().name(format!("livestage-thumbs-{n}")).spawn(move || {
                    // Each worker owns its decoders (FFmpeg contexts are per thread).
                    let backends = BackendRegistry::default();
                    while let Ok(key) = rx.recv() {
                        let (path, at_ms) = &key;
                        let image = match livestage_video::thumbnail::thumbnail(path, ShowTime::from_millis(*at_ms), MAX, &backends) {
                            Ok(f) => Some(egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.data)),
                            Err(e) => {
                                tracing::debug!("thumbnail {}: {e}", path.display());
                                None
                            }
                        };
                        if tx.send((key, image)).is_err() {
                            break;
                        }
                    }
                });
            }
            Workers { jobs, done }
        })
    }

    /// Uploads finished thumbnails. Call once per UI frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some(w) = &self.workers else { return };
        let finished: Vec<_> = w.done.try_iter().collect();
        for (key, image) in finished {
            let entry = match image {
                Some(img) => {
                    let name = format!("thumb:{}@{}", key.0.display(), key.1);
                    Entry::Ready(ctx.load_texture(name, img, egui::TextureOptions::LINEAR))
                }
                None => Entry::Failed,
            };
            self.map.insert(key, entry);
        }
    }

    /// The thumbnail of `path` (a frame `at` into movies), requesting it if needed.
    pub fn get(&mut self, path: &Path, at: ShowTime) -> Option<egui::TextureHandle> {
        let key = (path.to_path_buf(), at.as_micros() / 1000);
        match self.map.get(&key) {
            Some(Entry::Ready(t)) => Some(t.clone()),
            Some(Entry::Pending | Entry::Failed) => None,
            None => {
                if self.map.len() >= CAPACITY {
                    // Simple bound: drop everything finished and start over.
                    self.map.retain(|_, e| matches!(e, Entry::Pending));
                }
                let _ = self.workers().jobs.send(key.clone());
                self.map.insert(key, Entry::Pending);
                None
            }
        }
    }

    /// Whether a thumbnail could not be produced (unsupported media, missing file).
    pub fn failed(&self, path: &Path, at: ShowTime) -> bool {
        matches!(self.map.get(&(path.to_path_buf(), at.as_micros() / 1000)), Some(Entry::Failed))
    }

    pub fn pending(&self) -> usize {
        self.map.values().filter(|e| matches!(e, Entry::Pending)).count()
    }
}
