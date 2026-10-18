//! Slide playback: a [`VideoSource`] that shows one slide of a deck and
//! decodes slides on a worker thread, prefetching the neighbours so Next /
//! Previous never wait on PNG decoding.

use crate::source::{load_image, VideoFrame, VideoSource};
use livestage_core::time::ShowTime;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

/// Slides kept decoded around the current one (behind, ahead).
const KEEP_BEHIND: usize = 1;
const KEEP_AHEAD: usize = 2;

pub struct SlideSource {
    paths: Vec<PathBuf>,
    index: usize,
    /// Picture on screen; held until the requested slide is decoded.
    shown: Option<VideoFrame>,
    cache: HashMap<usize, VideoFrame>,
    requested: HashSet<usize>,
    requests: mpsc::Sender<(usize, PathBuf)>,
    decoded: mpsc::Receiver<(usize, Option<VideoFrame>)>,
    size: (u32, u32),
}

impl SlideSource {
    /// `preloaded` are already-decoded slides (e.g. from the still cache).
    pub fn new(paths: Vec<PathBuf>, index: usize, preloaded: HashMap<usize, VideoFrame>) -> Self {
        let (requests, rx) = mpsc::channel::<(usize, PathBuf)>();
        let (tx, decoded) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("livestage-slides".into()).spawn(move || {
            while let Ok((i, path)) = rx.recv() {
                let frame = match load_image(&path) {
                    Ok(f) => Some(f),
                    Err(e) => {
                        tracing::error!("slide {}: {e}", i + 1);
                        None
                    }
                };
                if tx.send((i, frame)).is_err() {
                    break;
                }
            }
        });
        if let Err(e) = spawned {
            tracing::error!("slide loader thread: {e}");
        }
        let size = preloaded.values().next().map_or((0, 0), |f| (f.width, f.height));
        let mut s = Self {
            paths,
            index,
            shown: None,
            cache: preloaded,
            requested: HashSet::new(),
            requests,
            decoded,
            size,
        };
        s.set_slide(index);
        s
    }

    pub fn index(&self) -> usize {
        self.index
    }

    fn request(&mut self, i: usize) {
        if i < self.paths.len() && !self.cache.contains_key(&i) && self.requested.insert(i) {
            let _ = self.requests.send((i, self.paths[i].clone()));
        }
    }

    fn drain(&mut self) {
        while let Ok((i, frame)) = self.decoded.try_recv() {
            self.requested.remove(&i);
            if let Some(f) = frame {
                self.size = (f.width, f.height);
                self.cache.insert(i, f);
            }
        }
        let keep = self.index.saturating_sub(KEEP_BEHIND)..=self.index + KEEP_AHEAD;
        self.cache.retain(|i, _| keep.contains(i));
        if let Some(f) = self.cache.get(&self.index) {
            self.shown = Some(f.clone());
        }
    }
}

impl VideoSource for SlideSource {
    fn frame_at(&mut self, _t: ShowTime) -> Option<VideoFrame> {
        self.drain();
        self.shown.clone()
    }
    fn duration(&self) -> Option<ShowTime> {
        None
    }
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn set_slide(&mut self, index: usize) {
        self.index = index.min(self.paths.len().saturating_sub(1));
        // Current slide first, then the direction an operator usually goes.
        self.request(self.index);
        for i in 1..=KEEP_AHEAD {
            self.request(self.index + i);
        }
        if let Some(i) = self.index.checked_sub(1) {
            self.request(i);
        }
        self.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_in_background_and_holds_until_ready() {
        let dir = std::env::temp_dir().join(format!("livestage-slidesrc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<PathBuf> = (0..4)
            .map(|i| {
                let p = dir.join(format!("{i}.png"));
                image::RgbaImage::from_pixel(4, 4, image::Rgba([i as u8 * 50, 0, 0, 255])).save(&p).unwrap();
                p
            })
            .collect();
        let mut src = SlideSource::new(paths, 0, HashMap::new());
        let wait = |src: &mut SlideSource, red: u8| {
            for _ in 0..200 {
                if src.frame_at(ShowTime::ZERO).is_some_and(|f| f.data[0] == red) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            false
        };
        assert!(wait(&mut src, 0));
        src.set_slide(2);
        assert!(wait(&mut src, 100));
        src.set_slide(99);
        assert_eq!(src.index(), 3);
        assert!(wait(&mut src, 150));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
