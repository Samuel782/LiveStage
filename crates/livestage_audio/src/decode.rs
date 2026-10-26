//! Universal decoding via Symphonia (WAV, AIFF, FLAC, MP3, AAC/M4A, OGG
//! Vorbis, ALAC, ...). Runs on loader threads, never on the audio thread.

use crate::{AudioClip, AudioError, AudioResult};
use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Lightweight probe result for import-time metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: Option<u64>,
}

impl AudioInfo {
    pub fn duration_secs(&self) -> Option<f64> {
        self.frames.map(|f| f as f64 / self.sample_rate.max(1) as f64)
    }
}

fn open(path: &Path) -> AudioResult<Box<dyn symphonia::core::formats::FormatReader>> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| AudioError::Decode { path: path.display().to_string(), msg: e.to_string() })?;
    Ok(probed.format)
}

/// Reads stream parameters without decoding.
pub fn probe(path: &Path) -> AudioResult<AudioInfo> {
    let format = open(path)?;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioError::NoTrack(path.display().to_string()))?;
    let p = &track.codec_params;
    Ok(AudioInfo {
        sample_rate: p.sample_rate.unwrap_or(48_000),
        channels: p.channels.map(|c| c.count() as u16).unwrap_or(2),
        frames: p.n_frames,
    })
}

/// Fully decodes a file to interleaved f32 at its native rate.
pub fn decode_file(path: &Path) -> AudioResult<AudioClip> {
    let mut format = open(path)?;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioError::NoTrack(path.display().to_string()))?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| AudioError::Decode { path: path.display().to_string(), msg: e.to_string() })?;

    let mut sample_rate = params.sample_rate.unwrap_or(48_000);
    let mut channels = params.channels.map(|c| c.count() as u16).unwrap_or(0);
    let mut data: Vec<f32> = match (params.n_frames, channels) {
        (Some(n), c) if c > 0 => Vec::with_capacity(n as usize * c as usize),
        _ => Vec::new(),
    };
    let mut sbuf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(AudioError::Decode { path: path.display().to_string(), msg: e.to_string() }),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                sample_rate = spec.rate;
                channels = spec.channels.count() as u16;
                let needed = buf.capacity() as u64;
                let sb = match &mut sbuf {
                    Some(sb) if sb.capacity() as u64 >= needed * spec.channels.count() as u64 => sb,
                    _ => sbuf.insert(SampleBuffer::<f32>::new(needed, spec)),
                };
                sb.copy_interleaved_ref(buf);
                data.extend_from_slice(sb.samples());
            }
            // Corrupt frames are skipped rather than aborting the show.
            Err(SymError::DecodeError(e)) => tracing::warn!(file = %path.display(), "skipping bad frame: {e}"),
            Err(e) => return Err(AudioError::Decode { path: path.display().to_string(), msg: e.to_string() }),
        }
    }

    if channels == 0 {
        return Err(AudioError::NoTrack(path.display().to_string()));
    }
    Ok(AudioClip { sample_rate, channels, data })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a minimal 16-bit PCM WAV.
    pub(crate) fn write_wav(path: &Path, rate: u32, channels: u16, frames: &[i16]) {
        let data_len = (frames.len() * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for s in frames {
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn decodes_wav() {
        let p = std::env::temp_dir().join(format!("ls-decode-{}.wav", std::process::id()));
        let samples: Vec<i16> = (0..2000).map(|i| if i % 2 == 0 { 16384 } else { -16384 }).collect();
        write_wav(&p, 44_100, 2, &samples);
        let info = probe(&p).unwrap();
        assert_eq!((info.sample_rate, info.channels), (44_100, 2));
        let clip = decode_file(&p).unwrap();
        assert_eq!(clip.frames(), 1000);
        assert!((clip.sample(10, 0) - 0.5).abs() < 1e-3);
        assert!((clip.sample(10, 1) + 0.5).abs() < 1e-3);
        std::fs::remove_file(p).ok();
    }
}
