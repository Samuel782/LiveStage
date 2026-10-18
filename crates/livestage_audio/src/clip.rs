//! Decoded, device-rate PCM held in memory.

/// Interleaved 32-bit float PCM at the engine sample rate.
///
/// Clips are immutable and shared via `Arc`, so the same file used by many
/// cues is decoded once. They are pre-resampled to the device rate at load
/// time so the real-time thread only ever indexes memory.
#[derive(Debug, Clone)]
pub struct AudioClip {
    pub sample_rate: u32,
    pub channels: u16,
    pub data: Vec<f32>,
}

impl AudioClip {
    pub fn frames(&self) -> u64 {
        if self.channels == 0 {
            0
        } else {
            (self.data.len() / self.channels as usize) as u64
        }
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }

    #[inline]
    pub fn sample(&self, frame: u64, channel: u16) -> f32 {
        let idx = frame as usize * self.channels as usize + channel as usize;
        // Bounds are checked by the caller; `get` keeps us panic-free regardless.
        self.data.get(idx).copied().unwrap_or(0.0)
    }

    /// Resamples with 4-point Catmull-Rom (cubic Hermite) interpolation.
    ///
    /// Good transparency for show playback at negligible cost. For mastering
    /// grade SRC, swap in a windowed-sinc resampler (e.g. `rubato`) here.
    pub fn resampled(&self, target_rate: u32) -> AudioClip {
        if target_rate == self.sample_rate || self.frames() == 0 || target_rate == 0 {
            return AudioClip { sample_rate: target_rate.max(self.sample_rate), ..self.clone() };
        }
        let ch = self.channels as usize;
        let in_frames = self.frames() as usize;
        let ratio = self.sample_rate as f64 / target_rate as f64;
        let out_frames = ((in_frames as f64) / ratio).floor() as usize;
        let mut data = Vec::with_capacity(out_frames * ch);
        let get = |f: isize, c: usize| -> f32 {
            let f = f.clamp(0, in_frames as isize - 1) as usize;
            self.data[f * ch + c]
        };
        for o in 0..out_frames {
            let pos = o as f64 * ratio;
            let i = pos.floor() as isize;
            let t = (pos - i as f64) as f32;
            for c in 0..ch {
                let (p0, p1, p2, p3) = (get(i - 1, c), get(i, c), get(i + 1, c), get(i + 2, c));
                let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
                let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
                let cc = -0.5 * p0 + 0.5 * p2;
                data.push(((a * t + b) * t + cc) * t + p1);
            }
        }
        AudioClip { sample_rate: target_rate, channels: self.channels, data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_preserves_duration_and_dc() {
        let clip = AudioClip { sample_rate: 44_100, channels: 2, data: vec![0.5; 44_100 * 2] };
        let r = clip.resampled(48_000);
        assert_eq!(r.frames(), 48_000);
        assert!(r.data.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }
}
