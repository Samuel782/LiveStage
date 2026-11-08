//! Soundtracks that Symphonia cannot read, decoded with FFmpeg (`ffmpeg`
//! cargo feature): movie containers such as AVI, MXF or MPEG-TS, and codecs
//! such as AC-3. Runs on the loader thread and decodes the whole track.
//!
//! The track is aligned to the movie's picture: the video stream's start is
//! time zero, so a soundtrack that starts later gets leading silence and one
//! that starts earlier is trimmed, exactly as the video backend times frames.

use crate::{AudioClip, AudioError, AudioResult};
use ffmpeg_next as ff;
use std::path::Path;
use std::sync::OnceLock;

fn err(path: &Path, e: impl std::fmt::Display) -> AudioError {
    crate::decode::decode_err(path, e)
}

fn init() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        ff::init().map_err(|e| e.to_string())?;
        ff::util::log::set_level(ff::util::log::Level::Error);
        Ok(())
    })
    .clone()
}

/// Whether FFmpeg finds a decodable audio stream in `path` (headers only).
pub fn has_audio_track(path: &Path) -> bool {
    init().is_ok()
        && ff::format::input(path).is_ok_and(|input| {
            input.streams().best(ff::media::Type::Audio).is_some_and(|s| ff::codec::decoder::find(s.parameters().id()).is_some())
        })
}

/// Decodes the best audio stream of `path` to interleaved f32 at its native rate.
pub fn decode_file(path: &Path) -> AudioResult<AudioClip> {
    init().map_err(|e| err(path, e))?;
    let mut input = ff::format::input(path).map_err(|e| err(path, e))?;
    let stream = input.streams().best(ff::media::Type::Audio).ok_or_else(|| AudioError::NoTrack(path.display().to_string()))?;
    let index = stream.index();
    let time_base = stream.time_base();
    // Time zero of the picture (or of the soundtrack in an audio-only file).
    let zero = input
        .streams()
        .best(ff::media::Type::Video)
        .map(|v| start_secs(v.start_time(), v.time_base()))
        .unwrap_or_else(|| start_secs(stream.start_time(), time_base));
    let ctx = ff::codec::context::Context::from_parameters(stream.parameters()).map_err(|e| err(path, e))?;
    let mut decoder = ctx.decoder().audio().map_err(|e| err(path, e))?;

    let mut packet = ff::Packet::empty();
    let mut frame = ff::frame::Audio::empty();
    let mut data: Vec<f32> = Vec::new();
    let (mut rate, mut channels) = (0u32, 0u16);
    // Frames still to drop (soundtrack earlier than the picture).
    let mut trim = 0usize;
    loop {
        let eof = match packet.read(&mut input) {
            Ok(()) => {
                if packet.stream() == index {
                    if let Err(e) = decoder.send_packet(&packet) {
                        tracing::debug!(file = %path.display(), "skipping bad audio packet: {e}");
                    }
                }
                false
            }
            Err(ff::Error::Eof) => {
                let _ = decoder.send_eof();
                true
            }
            Err(ff::Error::InvalidData) => continue,
            Err(e) => return Err(err(path, e)),
        };
        while decoder.receive_frame(&mut frame).is_ok() {
            if channels == 0 {
                rate = frame.rate();
                channels = frame.channels();
                if rate == 0 || channels == 0 {
                    return Err(err(path, "audio stream has no rate or channels"));
                }
                // Align the first frame with the picture.
                let first = frame.timestamp().or(frame.pts()).map_or(zero, |pts| pts as f64 * f64::from(time_base));
                let lead = ((first - zero) * rate as f64).round() as i64;
                if lead > 0 {
                    data.resize(lead as usize * channels as usize, 0.0);
                } else {
                    trim = lead.unsigned_abs() as usize;
                }
            }
            if frame.channels() != channels || frame.rate() != rate {
                // A mid-stream format change: the clip keeps its first layout.
                tracing::debug!(file = %path.display(), "skipping audio frame with a different layout");
                continue;
            }
            let before = data.len();
            append_interleaved(&frame, &mut data).map_err(|e| err(path, e))?;
            if trim > 0 {
                let drop = trim.min((data.len() - before) / channels as usize);
                data.drain(before..before + drop * channels as usize);
                trim -= drop;
            }
        }
        if eof {
            break;
        }
    }
    if channels == 0 {
        return Err(AudioError::NoTrack(path.display().to_string()));
    }
    Ok(AudioClip { sample_rate: rate, channels, data })
}

fn start_secs(start: i64, time_base: ff::Rational) -> f64 {
    if start == ff::ffi::AV_NOPTS_VALUE {
        0.0
    } else {
        start as f64 * f64::from(time_base)
    }
}

/// Appends a decoded frame as interleaved f32, whatever FFmpeg's sample format.
fn append_interleaved(frame: &ff::frame::Audio, out: &mut Vec<f32>) -> Result<(), String> {
    use ff::format::Sample;
    let fmt = frame.format();
    let read: fn(&[u8]) -> f32 = match fmt {
        Sample::U8(_) => |b| (b[0] as f32 - 128.0) / 128.0,
        Sample::I16(_) => |b| i16::from_ne_bytes([b[0], b[1]]) as f32 / 32_768.0,
        Sample::I32(_) => |b| i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        Sample::I64(_) => |b| (i64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64 / 9.223_372_036_854_776e18) as f32,
        Sample::F32(_) => |b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
        Sample::F64(_) => |b| f64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
        Sample::None => return Err("audio frame has no sample format".into()),
    };
    let (n, ch, size) = (frame.samples(), frame.channels() as usize, fmt.bytes());
    out.reserve(n * ch);
    if frame.is_planar() {
        let planes: Vec<&[u8]> = (0..ch).map(|c| frame.data(c)).collect();
        if planes.iter().any(|p| p.len() < n * size) {
            return Err("short audio plane".into());
        }
        for i in 0..n {
            for p in &planes {
                out.push(read(&p[i * size..]));
            }
        }
    } else {
        let d = frame.data(0);
        if d.len() < n * ch * size {
            return Err("short audio buffer".into());
        }
        for i in 0..n * ch {
            out.push(read(&d[i * size..]));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::AudioEngine;
    use livestage_core::action::{AudioAction, AudioTrack};
    use livestage_core::project::AssetRef;
    use livestage_core::routing::AudioRouting;
    use livestage_core::{CueId, ShowTime};
    use std::path::PathBuf;

    const RATE: i32 = 48_000;
    /// The soundtrack is silent until here, then a 440 Hz tone.
    const ONSET_SECS: f64 = 0.25;
    const SECS: f64 = 1.0;

    fn tone(i: i64, channel: usize) -> f32 {
        let t = i as f64 / RATE as f64;
        if t < ONSET_SECS {
            0.0
        } else {
            ((t * 440.0 * std::f64::consts::TAU).sin() * if channel == 0 { 0.5 } else { 0.25 }) as f32
        }
    }

    /// Writes a 1 s, 10 fps 32x32 movie (MPEG-4 video) with a stereo
    /// soundtrack in `audio` (`None`: no audio track). `None` when this
    /// FFmpeg build lacks an encoder or muxer.
    pub(crate) fn write_movie(path: &Path, audio: Option<(ff::codec::Id, ff::format::Sample)>) -> Option<()> {
        init().ok()?;
        let mut out = ff::format::output(path).ok()?;
        let global = out.format().flags().contains(ff::format::Flags::GLOBAL_HEADER);

        let vcodec = ff::encoder::find(ff::codec::Id::MPEG4)?;
        let mut venc = ff::codec::context::Context::new_with_codec(vcodec).encoder().video().ok()?;
        venc.set_width(32);
        venc.set_height(32);
        venc.set_format(ff::format::Pixel::YUV420P);
        venc.set_time_base((1, 10));
        venc.set_frame_rate(Some((10, 1)));
        if global {
            venc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        }
        let mut venc = venc.open_as(vcodec).ok()?;
        let vindex = {
            let mut st = out.add_stream(vcodec).ok()?;
            st.set_parameters(&venc);
            st.set_time_base((1, 10));
            st.index()
        };
        let mut aenc = match audio {
            Some((id, format)) => {
                let acodec = ff::encoder::find(id)?;
                let mut a = ff::codec::context::Context::new_with_codec(acodec).encoder().audio().ok()?;
                a.set_rate(RATE);
                a.set_channel_layout(ff::ChannelLayout::STEREO);
                a.set_format(format);
                a.set_bit_rate(128_000);
                a.set_time_base((1, RATE));
                if global {
                    a.set_flags(ff::codec::Flags::GLOBAL_HEADER);
                }
                let a = a.open_as(acodec).ok()?;
                let index = {
                    let mut st = out.add_stream(acodec).ok()?;
                    st.set_parameters(&a);
                    st.set_time_base((1, RATE));
                    st.index()
                };
                Some((a, index))
            }
            None => None,
        };
        out.write_header().ok()?;
        let vtb = out.stream(vindex)?.time_base();
        let atb = aenc.as_ref().and_then(|(_, i)| out.stream(*i)).map(|s| s.time_base());

        let mut packet = ff::Packet::empty();
        for i in 0..10 {
            let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, 32, 32);
            frame.data_mut(0).fill(16 + i as u8 * 20);
            frame.data_mut(1).fill(128);
            frame.data_mut(2).fill(128);
            frame.set_pts(Some(i));
            venc.send_frame(&frame).ok()?;
            while venc.receive_packet(&mut packet).is_ok() {
                packet.set_stream(vindex);
                packet.rescale_ts((1, 10), vtb);
                packet.write_interleaved(&mut out).ok()?;
            }
        }
        venc.send_eof().ok()?;
        while venc.receive_packet(&mut packet).is_ok() {
            packet.set_stream(vindex);
            packet.rescale_ts((1, 10), vtb);
            packet.write_interleaved(&mut out).ok()?;
        }

        if let (Some((enc, index)), Some(atb)) = (aenc.as_mut(), atb) {
            let chunk = match enc.frame_size() {
                0 => 1024,
                n => n as usize,
            };
            let format = enc.format();
            let size = format.bytes();
            let total = (SECS * RATE as f64) as i64;
            let mut at = 0i64;
            let mut drain = |enc: &mut ff::encoder::Audio, out: &mut ff::format::context::Output| -> Option<()> {
                while enc.receive_packet(&mut packet).is_ok() {
                    packet.set_stream(*index);
                    packet.rescale_ts((1, RATE), atb);
                    packet.write_interleaved(out).ok()?;
                }
                Some(())
            };
            while at < total {
                let n = chunk.min((total - at) as usize);
                let mut frame = ff::frame::Audio::new(format, n, ff::ChannelLayout::STEREO);
                frame.set_rate(RATE as u32);
                for i in 0..n {
                    for ch in 0..2 {
                        let v = tone(at + i as i64, ch);
                        let bytes: Vec<u8> = match format {
                            ff::format::Sample::F32(_) => v.to_ne_bytes().to_vec(),
                            ff::format::Sample::I16(_) => ((v * 32_767.0) as i16).to_ne_bytes().to_vec(),
                            _ => return None,
                        };
                        let (plane, offset) = if frame.is_planar() { (ch, i * size) } else { (0, (i * 2 + ch) * size) };
                        frame.data_mut(plane)[offset..offset + size].copy_from_slice(&bytes);
                    }
                }
                frame.set_pts(Some(at));
                enc.send_frame(&frame).ok()?;
                drain(enc, &mut out)?;
                at += n as i64;
            }
            enc.send_eof().ok()?;
            drain(enc, &mut out)?;
        }
        out.write_trailer().ok()?;
        Some(())
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("livestage-movie-audio-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// When the tone starts in a decoded clip, in seconds.
    fn onset(clip: &AudioClip) -> f64 {
        let ch = clip.channels as usize;
        let first = clip.data.chunks(ch).position(|f| f[0].abs() > 0.05).expect("tone in the soundtrack");
        first as f64 / clip.sample_rate as f64
    }

    fn check_soundtrack(clip: &AudioClip, what: &str) {
        assert_eq!((clip.sample_rate, clip.channels), (RATE as u32, 2), "{what}");
        let secs = clip.duration_secs();
        assert!((secs - SECS).abs() < 0.1, "{what}: {secs} s");
        let at = onset(clip);
        assert!((at - ONSET_SECS).abs() < 0.03, "{what}: tone starts at {at} s, not {ONSET_SECS} s");
        // Left at 0.5, right at 0.25: the channels are not swapped or mixed.
        let peak = |c: usize| clip.data.chunks(2).map(|f| f[c].abs()).fold(0.0f32, f32::max);
        assert!((peak(0) - 0.5).abs() < 0.08 && (peak(1) - 0.25).abs() < 0.08, "{what}: peaks {} / {}", peak(0), peak(1));
    }

    #[test]
    fn decodes_movie_soundtracks() {
        let dir = temp_dir("decode");
        use ff::format::sample::Type::{Packed, Planar};
        use ff::format::Sample;
        // AAC in MP4 (read by Symphonia), PCM in AVI (only FFmpeg reads AVI).
        let cases = [("aac.mp4", ff::codec::Id::AAC, Sample::F32(Planar)), ("pcm.avi", ff::codec::Id::PCM_S16LE, Sample::I16(Packed))];
        let mut tested = 0;
        for (name, codec, format) in cases {
            let path = dir.join(name);
            if write_movie(&path, Some((codec, format))).is_none() {
                eprintln!("skipping {name}: encoder or muxer unavailable");
                continue;
            }
            assert!(crate::decode::has_audio_track(&path), "{name}");
            let clip = crate::decode::decode_file(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            check_soundtrack(&clip, name);
            check_soundtrack(&decode_file(&path).unwrap(), &format!("{name} via FFmpeg"));
            tested += 1;
        }
        assert!(tested > 0, "no test movie could be written");
        assert!(crate::decode::decode_symphonia(&dir.join("pcm.avi")).is_err(), "AVI needs the FFmpeg fallback");

        let silent = dir.join("silent.mp4");
        if write_movie(&silent, None).is_some() {
            assert!(!crate::decode::has_audio_track(&silent));
            assert!(matches!(crate::decode::decode_file(&silent), Err(AudioError::NoTrack(_))));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Renders an offline engine (no device, nothing reaches the speakers)
    /// until `cue`'s clock runs; returns the buffers rendered from then on.
    fn render_from_start(engine: &mut AudioEngine, device: &mut crate::Renderer, cue: CueId, secs: f64) -> Vec<f32> {
        let mut buf = vec![0.0f32; 256 * 2];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let clock = engine.voice_clock(cue).expect("voice registered at GO");
        loop {
            engine.maintenance();
            device.render(&mut buf);
            if clock.read().running {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the soundtrack never started");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut out = buf.clone();
        while out.len() < (secs * RATE as f64) as usize * 2 {
            device.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    }

    fn play(cue: CueId, asset: &str, stem: livestage_core::StemId) -> AudioAction {
        AudioAction::Play {
            cue,
            tracks: vec![AudioTrack { asset: AssetRef::new(asset), stem, gain_db: 0.0, mute: false }],
            click: None,
            gain_db: 0.0,
            fade_in: ShowTime::ZERO,
            start_at: ShowTime::ZERO,
            end_at: None,
            looped: false,
        }
    }

    /// A movie's soundtrack through the engine: routed to a stem, in time
    /// with its sample clock, and faded out by a stop.
    #[test]
    fn engine_plays_and_stops_a_movie_soundtrack() {
        let dir = temp_dir("engine");
        let movie = dir.join("movie.mp4");
        if write_movie(&movie, Some((ff::codec::Id::AAC, ff::format::Sample::F32(ff::format::sample::Type::Planar)))).is_none() {
            eprintln!("skipping: no AAC encoder");
            return;
        }
        let routing = AudioRouting::default();
        let (mut engine, mut device) = AudioEngine::offline(RATE as u32, 2, &routing).unwrap();
        let resolve = |a: &AssetRef| Some(dir.join(a.as_str()));
        let cue = CueId::new();
        engine.handle(&play(cue, "movie.mp4", routing.stems[0].id), &resolve);
        let out = render_from_start(&mut engine, &mut device, cue, 0.6);
        let rms = |from: f64, to: f64| {
            let s = &out[(from * RATE as f64) as usize * 2..(to * RATE as f64) as usize * 2];
            (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt()
        };
        assert!(rms(0.0, 0.2) < 0.01, "silent before the tone: {}", rms(0.0, 0.2));
        assert!(rms(0.3, 0.55) > 0.1, "tone after {ONSET_SECS} s: {}", rms(0.3, 0.55));
        let pos = engine.media_position(cue).unwrap().as_secs_f64();
        assert!((pos - out.len() as f64 / 2.0 / RATE as f64).abs() < 0.01, "clock {pos}");

        engine.handle(&AudioAction::Stop { cue, fade: ShowTime::from_millis(50) }, &resolve);
        let mut buf = vec![0.0f32; 256 * 2];
        for _ in 0..20 {
            device.render(&mut buf);
        }
        assert!(buf.iter().all(|v| *v == 0.0), "stopped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Movie audio switched on for a movie without sound: the voice plays
    /// silence and its clock runs, so a picture locked to it never freezes.
    #[test]
    fn silent_movie_keeps_a_running_clock() {
        let dir = temp_dir("silent");
        if write_movie(&dir.join("silent.mp4"), None).is_none() {
            return;
        }
        let routing = AudioRouting::default();
        let (mut engine, mut device) = AudioEngine::offline(RATE as u32, 2, &routing).unwrap();
        let resolve = |a: &AssetRef| Some(dir.join(a.as_str()));
        let cue = CueId::new();
        engine.handle(&play(cue, "silent.mp4", routing.stems[0].id), &resolve);
        let out = render_from_start(&mut engine, &mut device, cue, 0.3);
        assert!(out.iter().all(|v| *v == 0.0));
        let r = engine.voice_clock(cue).unwrap().read();
        assert!(r.running && !r.ended && r.position > ShowTime::from_millis(250), "{r:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
