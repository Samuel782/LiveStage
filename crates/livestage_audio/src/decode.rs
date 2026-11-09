//! Universal decoding via Symphonia (WAV, AIFF, FLAC, MP3, AAC/M4A, OGG
//! Vorbis, ALAC, ...), including the soundtracks of MP4 / MOV / MKV movies.
//! With the `ffmpeg` feature, files Symphonia cannot read fall back to
//! FFmpeg (see [`crate::ffmpeg_decode`]). Runs on loader threads, never on
//! the audio thread.

use crate::{AudioClip, AudioError, AudioResult};
use std::fs::File;
use std::path::Path;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::TimeBase;

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

pub(crate) fn decode_err(path: &Path, e: impl std::fmt::Display) -> AudioError {
    AudioError::Decode { path: path.display().to_string(), msg: e.to_string() }
}

/// The first audio track of an opened file.
pub(crate) struct AudioTrack {
    pub id: u32,
    pub params: AudioCodecParameters,
    pub time_base: Option<TimeBase>,
    pub frames: Option<u64>,
}

/// Opens `path` and finds its first audio track.
pub(crate) fn open(path: &Path) -> AudioResult<(Box<dyn FormatReader>, AudioTrack)> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| decode_err(path, e))?;
    let track = format
        .tracks()
        .iter()
        .find_map(|t| match &t.codec_params {
            Some(CodecParameters::Audio(p)) => Some(AudioTrack { id: t.id, params: p.clone(), time_base: t.time_base, frames: t.num_frames }),
            _ => None,
        })
        .ok_or_else(|| AudioError::NoTrack(path.display().to_string()))?;
    Ok((format, track))
}

/// A decoder for `track`. Gapless trimming stays off (as in Symphonia
/// 0.5): packet timestamps then map 1:1 to decoded frames, which the
/// sample-accurate seek in `stream` relies on.
pub(crate) fn make_decoder(path: &Path, track: &AudioTrack) -> AudioResult<Box<dyn AudioDecoder>> {
    let mut opts = AudioDecoderOptions::default();
    opts.gapless = false;
    symphonia::default::get_codecs().make_audio_decoder(&track.params, &opts).map_err(|e| decode_err(path, e))
}

/// Reads stream parameters without decoding.
pub fn probe(path: &Path) -> AudioResult<AudioInfo> {
    let (_, track) = open(path)?;
    let p = &track.params;
    Ok(AudioInfo { sample_rate: p.sample_rate.unwrap_or(48_000), channels: p.channels.as_ref().map(|c| c.count() as u16).unwrap_or(2), frames: track.frames })
}

/// Whether `path` has an audio track that can be played (a movie's
/// soundtrack). Reads headers only.
pub fn has_audio_track(path: &Path) -> bool {
    match open(path) {
        Ok((_, track)) => {
            // Symphonia found a track; can it (or FFmpeg) decode it?
            make_decoder(path, &track).is_ok() || ffmpeg_has_audio(path)
        }
        Err(AudioError::NoTrack(_)) => false,
        Err(_) => ffmpeg_has_audio(path),
    }
}

#[cfg(feature = "ffmpeg")]
fn ffmpeg_has_audio(path: &Path) -> bool {
    crate::ffmpeg_decode::has_audio_track(path)
}

#[cfg(not(feature = "ffmpeg"))]
fn ffmpeg_has_audio(_: &Path) -> bool {
    false
}

/// Fully decodes a file's first audio track to interleaved f32 at its
/// native rate: Symphonia first, then FFmpeg when built with `ffmpeg`.
pub fn decode_file(path: &Path) -> AudioResult<AudioClip> {
    match decode_symphonia(path) {
        // No audio track at all: FFmpeg would not find one either.
        Err(e @ AudioError::NoTrack(_)) => Err(e),
        #[cfg(feature = "ffmpeg")]
        Err(e) => {
            tracing::debug!(file = %path.display(), "Symphonia cannot decode ({e}); trying FFmpeg");
            crate::ffmpeg_decode::decode_file(path)
        }
        other => other,
    }
}

/// [`decode_file`] with Symphonia only.
pub(crate) fn decode_symphonia(path: &Path) -> AudioResult<AudioClip> {
    let (mut format, track) = open(path)?;
    let mut decoder = make_decoder(path, &track)?;

    let mut sample_rate = track.params.sample_rate.unwrap_or(48_000);
    let mut channels = track.params.channels.as_ref().map(|c| c.count() as u16).unwrap_or(0);
    let mut data: Vec<f32> = match (track.frames, channels) {
        (Some(n), c) if c > 0 => Vec::with_capacity(n as usize * c as usize),
        _ => Vec::new(),
    };
    let mut scratch: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(decode_err(path, e)),
        };
        if packet.track_id != track.id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                sample_rate = spec.rate();
                channels = spec.channels().count() as u16;
                buf.copy_to_vec_interleaved(&mut scratch);
                data.extend_from_slice(&scratch);
            }
            // Corrupt frames are skipped rather than aborting the show.
            Err(SymError::DecodeError(e)) => tracing::warn!(file = %path.display(), "skipping bad frame: {e}"),
            Err(e) => return Err(decode_err(path, e)),
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
