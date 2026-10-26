//! cpal device discovery and stream construction.

use crate::renderer::{EngineShared, Renderer};
use crate::{AudioError, AudioResult};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub struct OutputDeviceInfo {
    pub name: String,
    pub max_channels: u16,
    pub default_sample_rate: u32,
    pub is_default: bool,
}

/// Enumerates output devices on the default host (CoreAudio / WASAPI / ALSA).
pub fn list_output_devices() -> Vec<OutputDeviceInfo> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());
    let Ok(devices) = host.output_devices() else { return vec![] };
    devices
        .filter_map(|d| {
            let name = d.name().ok()?;
            let max_channels = d.supported_output_configs().ok()?.map(|c| c.channels()).max().unwrap_or(0);
            let default_sample_rate = d.default_output_config().map(|c| c.sample_rate().0).unwrap_or(48_000);
            Some(OutputDeviceInfo {
                is_default: default_name.as_deref() == Some(name.as_str()),
                name,
                max_channels,
                default_sample_rate,
            })
        })
        .collect()
}

pub(crate) struct OpenedDevice {
    pub device: cpal::Device,
    pub name: String,
    pub config: StreamConfig,
    pub format: SampleFormat,
}

/// Chooses a device and the widest channel configuration at the desired rate.
pub(crate) fn select_device(
    device_name: Option<&str>,
    sample_rate: Option<u32>,
    buffer_frames: Option<u32>,
    channels: Option<u16>,
) -> AudioResult<OpenedDevice> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(want) => host
            .output_devices()
            .map_err(|e| AudioError::Device(e.to_string()))?
            .find(|d| d.name().map(|n| n == want).unwrap_or(false))
            .or_else(|| {
                tracing::warn!(device = want, "configured audio device not found, using default");
                host.default_output_device()
            }),
        None => host.default_output_device(),
    }
    .ok_or_else(|| AudioError::Device("no output device available".into()))?;
    let name = device.name().unwrap_or_else(|_| "Unknown device".into());

    let default = device.default_output_config().map_err(|e| AudioError::Device(e.to_string()))?;
    let rate = sample_rate.unwrap_or(default.sample_rate().0);

    // Prefer f32, then the most channels, at the requested rate.
    let ranges: Vec<_> = device.supported_output_configs().map_err(|e| AudioError::Device(e.to_string()))?.collect();
    let best = ranges
        .iter()
        .filter(|r| r.min_sample_rate().0 <= rate && rate <= r.max_sample_rate().0)
        .filter(|r| channels.is_none_or(|c| r.channels() >= c))
        .max_by_key(|r| (r.sample_format() == SampleFormat::F32, r.channels()));

    let (mut config, format, buffer_range) = match best {
        Some(r) => {
            let c = r.with_sample_rate(cpal::SampleRate(rate));
            (c.config(), c.sample_format(), Some(*c.buffer_size()))
        }
        None => (default.config(), default.sample_format(), Some(*default.buffer_size())),
    };
    if let Some(c) = channels {
        config.channels = config.channels.min(c.max(1)).max(1);
    }
    if let Some(frames) = buffer_frames {
        let ok = match buffer_range {
            Some(cpal::SupportedBufferSize::Range { min, max }) => (min..=max).contains(&frames),
            _ => true,
        };
        if ok {
            config.buffer_size = cpal::BufferSize::Fixed(frames);
        }
    }
    Ok(OpenedDevice { device, name, config, format })
}

pub(crate) fn build_stream(opened: &OpenedDevice, renderer: Renderer, shared: Arc<EngineShared>) -> AudioResult<cpal::Stream> {
    match opened.format {
        SampleFormat::F32 => build::<f32>(opened, renderer, shared),
        SampleFormat::I16 => build::<i16>(opened, renderer, shared),
        SampleFormat::U16 => build::<u16>(opened, renderer, shared),
        SampleFormat::I32 => build::<i32>(opened, renderer, shared),
        SampleFormat::F64 => build::<f64>(opened, renderer, shared),
        other => Err(AudioError::Device(format!("unsupported sample format {other:?}"))),
    }
}

fn build<T>(opened: &OpenedDevice, mut renderer: Renderer, shared: Arc<EngineShared>) -> AudioResult<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = opened.config.channels as usize;
    let rate = opened.config.sample_rate.0 as f64;
    // Scratch is allocated here, before the closure moves to the RT thread.
    let mut scratch = vec![0.0f32; 8192 * ch];
    let errors = shared.clone();
    let on_error = move |e| {
        errors.device_errors.fetch_add(1, Ordering::Relaxed);
        tracing::error!("audio stream error: {e}");
    };
    let config = opened.config.clone();

    let callback = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        let t0 = Instant::now();
        for chunk in data.chunks_mut(scratch.len()) {
            let s = &mut scratch[..chunk.len()];
            renderer.render(s);
            for (d, v) in chunk.iter_mut().zip(s.iter()) {
                *d = T::from_sample(*v);
            }
        }
        let frames = data.len() / ch.max(1);
        shared.callback_frames.store(frames as u32, Ordering::Relaxed);
        shared.callback_at_ns.store(shared.epoch.elapsed().as_nanos() as u64, Ordering::Release);
        let period = frames as f64 / rate;
        if period > 0.0 {
            let load = t0.elapsed().as_secs_f64() / period;
            shared.load_permille.store((load * 1000.0) as u32, Ordering::Relaxed);
        }
    };
    opened
        .device
        .build_output_stream(&config, callback, on_error, None)
        .map_err(|e| AudioError::Device(e.to_string()))
}
