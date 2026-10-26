//! Per-output layer stacks driven by core `VideoAction`s.
//!
//! The compositor is GPU-agnostic state: what is on which output, at which
//! opacity, at what media time. [`OutputRenderer`](crate::OutputRenderer)
//! turns it into pixels; text-like layers become [`Overlay`]s which the UI
//! draws with its font engine into the same canvas.

use crate::pipelines::GpuLayer;
use crate::source::{BackendRegistry, StillSource, VideoFrame, VideoSource};
use crate::sync::{Correction, LayerClock, LayerSync, MasterClock};
use livestage_core::action::{TextContent, TimerContent, TimerMode, VideoAction, VisualContent};
use livestage_core::id::{CueId, OutputId};
use livestage_core::project::AssetRef;
use livestage_core::time::ShowTime;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct Fade {
    from: f32,
    to: f32,
    start: ShowTime,
    duration: ShowTime,
}

impl Fade {
    pub fn constant(v: f32) -> Self {
        Self { from: v, to: v, start: ShowTime::ZERO, duration: ShowTime::ZERO }
    }
    pub fn value(&self, now: ShowTime) -> f32 {
        if self.duration.is_zero() || now >= self.start + self.duration {
            return self.to;
        }
        let t = (now - self.start).as_secs_f32() / self.duration.as_secs_f32();
        self.from + (self.to - self.from) * t.clamp(0.0, 1.0)
    }
    pub fn retarget(&mut self, now: ShowTime, to: f32, duration: ShowTime) {
        *self = Self { from: self.value(now), to, start: now, duration };
    }
    pub fn done(&self, now: ShowTime) -> bool {
        now >= self.start + self.duration
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OverlayKind {
    Text(TextContent),
    Timer(TimerContent),
    /// Operator-facing notice (e.g. "NDI source not available").
    Notice(String),
}

pub enum LayerContent {
    Source(Box<dyn VideoSource>),
    Overlay(OverlayKind),
}

pub struct Layer {
    pub cue: CueId,
    pub z: i32,
    pub opacity: Fade,
    pub content: LayerContent,
    clock: LayerClock,
    /// Master clock this layer is frame-locked to.
    pub sync: Option<LayerSync>,
    removing: bool,
    /// A fading copy of the previous picture during a slide cross-fade.
    ghost: bool,
    /// GPU resources, created lazily by the renderer.
    pub gpu: Option<GpuLayer>,
}

impl Layer {
    pub fn elapsed(&self, now: ShowTime) -> ShowTime {
        self.clock.at(now)
    }

    /// Current frame for texture layers.
    pub fn frame(&mut self, now: ShowTime) -> Option<VideoFrame> {
        let t = self.elapsed(now);
        match &mut self.content {
            LayerContent::Source(s) => s.frame_at(t),
            LayerContent::Overlay(_) => None,
        }
    }

    pub fn is_texture(&self) -> bool {
        matches!(self.content, LayerContent::Source(_))
    }
}

/// A resolved text-like layer ready to be drawn by the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    pub cue: CueId,
    pub z: i32,
    pub text: String,
    pub label: Option<String>,
    pub font_size: f32,
    pub color: [f32; 4],
    pub opacity: f32,
    /// Teleprompter scroll offset in points.
    pub scroll: f32,
    pub mirror: bool,
    /// Large tabular figures (timers).
    pub monospace: bool,
}

#[derive(Default)]
pub struct Compositor {
    outputs: HashMap<OutputId, Vec<Layer>>,
    cue_output: HashMap<CueId, OutputId>,
    finished: std::collections::HashSet<CueId>,
    pub backends: BackendRegistry,
    stills: HashMap<PathBuf, VideoFrame>,
}

impl Compositor {
    fn open_content(&mut self, content: &VisualContent, resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>) -> LayerContent {
        let notice = |m: String| LayerContent::Overlay(OverlayKind::Notice(m));
        match content {
            VisualContent::Video { asset, looped } => match resolve(asset) {
                Some(path) => match self.backends.open(&path, *looped) {
                    Ok(src) => LayerContent::Source(src),
                    Err(e) => {
                        tracing::error!("{e}");
                        notice(format!("VIDEO UNAVAILABLE\n{}", asset.file_name()))
                    }
                },
                None => notice(format!("MISSING ASSET\n{asset}")),
            },
            VisualContent::Image { asset } => match resolve(asset) {
                Some(path) => {
                    let frame = match self.stills.get(&path) {
                        Some(f) => Ok(f.clone()),
                        None => crate::source::load_image(&path).inspect(|f| {
                            self.stills.insert(path.clone(), f.clone());
                        }),
                    };
                    match frame {
                        Ok(f) => LayerContent::Source(Box::new(StillSource::new(f))),
                        Err(e) => {
                            tracing::error!("{e}");
                            notice(format!("IMAGE UNAVAILABLE\n{}", asset.file_name()))
                        }
                    }
                }
                None => notice(format!("MISSING ASSET\n{asset}")),
            },
            VisualContent::Color { rgba } => {
                let px = rgba.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                LayerContent::Source(Box::new(StillSource::new(VideoFrame::solid(px))))
            }
            VisualContent::TestPattern => LayerContent::Source(Box::new(StillSource::new(VideoFrame::test_pattern(1920, 1080)))),
            VisualContent::Text(t) => LayerContent::Overlay(OverlayKind::Text(t.clone())),
            VisualContent::Timer(t) => LayerContent::Overlay(OverlayKind::Timer(t.clone())),
            VisualContent::Slides { slides, index } => {
                let paths: Vec<PathBuf> = slides.iter().filter_map(resolve).collect();
                if paths.len() != slides.len() || paths.is_empty() {
                    return notice("MISSING SLIDES\nre-import the presentation".into());
                }
                // Slides preloaded at standby are handed over already decoded.
                let preloaded = paths
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| self.stills.get(p).map(|f| (i, f.clone())))
                    .collect();
                LayerContent::Source(Box::new(crate::slides::SlideSource::new(paths, *index, preloaded)))
            }
            VisualContent::Ndi { source_name } => match crate::ndi::NdiVideoSource::connect(source_name) {
                Some(src) => LayerContent::Source(Box::new(src)),
                None => notice(format!("NDI RUNTIME NOT INSTALLED\n{source_name}")),
            },
        }
    }

    fn layer_mut(&mut self, cue: CueId) -> Option<&mut Layer> {
        let out = self.cue_output.get(&cue)?;
        self.outputs.get_mut(out)?.iter_mut().find(|l| l.cue == cue && !l.ghost)
    }

    pub fn handle(&mut self, now: ShowTime, action: &VideoAction, resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>) {
        match action {
            VideoAction::Show { cue, output, content, layer, opacity, fade_in, start_at, sync } => {
                self.remove_cue(*cue);
                self.finished.remove(cue);
                let content = self.open_content(content, resolve);
                let mut fade = Fade::constant(if fade_in.is_zero() { *opacity } else { 0.0 });
                if !fade_in.is_zero() {
                    fade.retarget(now, *opacity, *fade_in);
                }
                let stack = self.outputs.entry(*output).or_default();
                stack.push(Layer {
                    cue: *cue,
                    z: *layer,
                    opacity: fade,
                    content,
                    clock: LayerClock::start(now, *start_at),
                    sync: sync.map(|s| LayerSync::new(s.master, s.offset)),
                    removing: false,
                    ghost: false,
                    gpu: None,
                });
                // Stable sort keeps GO order within the same z.
                stack.sort_by_key(|l| l.z);
                self.cue_output.insert(*cue, *output);
            }
            VideoAction::Hide { cue, fade } => {
                if let Some(l) = self.layer_mut(*cue) {
                    l.removing = true;
                    l.opacity.retarget(now, 0.0, *fade);
                }
            }
            VideoAction::Pause { cue } => {
                if let Some(l) = self.layer_mut(*cue) {
                    l.clock.pause(now);
                }
            }
            VideoAction::Resume { cue } => {
                if let Some(l) = self.layer_mut(*cue) {
                    l.clock.resume(now);
                }
            }
            VideoAction::Seek { cue, position } => {
                self.finished.remove(cue);
                if let Some(l) = self.layer_mut(*cue) {
                    let at = l.clock.paused_at().unwrap_or(now);
                    l.clock.seek(now, *position);
                    // Timers display wall-clock time since their start: re-base it.
                    if let LayerContent::Overlay(OverlayKind::Timer(t)) = &mut l.content {
                        t.started_at = at;
                        t.elapsed_offset = *position;
                    }
                }
            }
            VideoAction::GotoSlide { cue, index, fade } => {
                let Some(out) = self.cue_output.get(cue).copied() else { return };
                let Some(stack) = self.outputs.get_mut(&out) else { return };
                let Some(pos) = stack.iter().position(|l| l.cue == *cue && !l.ghost) else { return };
                let layer = &mut stack[pos];
                let ghost_frame = if fade.is_zero() { None } else { layer.frame(now) };
                if let LayerContent::Source(s) = &mut layer.content {
                    s.set_slide(*index);
                }
                if let Some(frame) = ghost_frame {
                    // Cross-fade: the old slide sits on top and fades out over the new one.
                    let mut opacity = Fade::constant(layer.opacity.value(now));
                    opacity.retarget(now, 0.0, *fade);
                    let ghost = Layer {
                        cue: *cue,
                        z: layer.z,
                        opacity,
                        content: LayerContent::Source(Box::new(StillSource::new(frame))),
                        clock: LayerClock::start(now, ShowTime::ZERO),
                        sync: None,
                        removing: true,
                        ghost: true,
                        gpu: None,
                    };
                    stack.insert(pos + 1, ghost);
                }
            }
            VideoAction::SetOpacity { cue, opacity, fade } => {
                if let Some(l) = self.layer_mut(*cue) {
                    l.opacity.retarget(now, *opacity, *fade);
                }
            }
            VideoAction::Preload { assets } => {
                for a in assets {
                    let Some(path) = resolve(a) else { continue };
                    let is_still = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"));
                    if is_still && !self.stills.contains_key(&path) {
                        match crate::source::load_image(&path) {
                            Ok(f) => {
                                self.stills.insert(path, f);
                            }
                            Err(e) => tracing::warn!("preload: {e}"),
                        }
                    }
                }
            }
            VideoAction::BlackoutAll { fade } => {
                for stack in self.outputs.values_mut() {
                    for l in stack {
                        l.removing = true;
                        l.opacity.retarget(now, 0.0, *fade);
                    }
                }
            }
        }
    }

    fn remove_cue(&mut self, cue: CueId) {
        if let Some(out) = self.cue_output.remove(&cue) {
            if let Some(stack) = self.outputs.get_mut(&out) {
                stack.retain(|l| l.cue != cue);
            }
        }
    }

    /// Masters that frame-locked layers follow (fetch their clocks for
    /// [`follow_masters`](Self::follow_masters)).
    pub fn sync_masters(&self) -> Vec<CueId> {
        let mut v: Vec<CueId> = self.outputs.values().flatten().filter_map(|l| l.sync.map(|s| s.master)).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Re-times frame-locked layers against their masters for the frame
    /// shown at `now`. Call once per frame, before rendering.
    pub fn follow_masters(&mut self, now: ShowTime, master: impl Fn(CueId) -> MasterClock) -> SyncReport {
        let mut report = SyncReport::default();
        for l in self.outputs.values_mut().flatten() {
            let Some(sync) = &mut l.sync else { continue };
            let c = sync.follow(&mut l.clock, now, master(sync.master));
            if c == Correction::Snapped {
                tracing::debug!(cue = %l.cue, error_ms = sync.last_error * 1e3, "video layer re-timed to its audio master");
            }
            report.layers += 1;
            if c != Correction::FreeRun {
                report.locked += 1;
                report.worst_error = report.worst_error.max(sync.last_error.abs());
            }
            report.snaps += (c == Correction::Snapped) as u32;
        }
        report
    }

    /// Drops faded-out layers and flags finished media. Call once per frame.
    pub fn update(&mut self, now: ShowTime) {
        for stack in self.outputs.values_mut() {
            for l in stack.iter() {
                if let LayerContent::Source(s) = &l.content {
                    if s.duration().is_some_and(|d| l.elapsed(now) >= d) {
                        self.finished.insert(l.cue);
                    }
                }
            }
            stack.retain(|l| !(l.removing && l.opacity.done(now)));
        }
        let alive: std::collections::HashSet<CueId> = self.outputs.values().flatten().map(|l| l.cue).collect();
        self.cue_output.retain(|c, _| alive.contains(c));
    }

    pub fn layers_mut(&mut self, output: OutputId) -> &mut [Layer] {
        self.outputs.get_mut(&output).map(|v| v.as_mut_slice()).unwrap_or(&mut [])
    }

    pub fn overlays(&self, output: OutputId, now: ShowTime) -> Vec<Overlay> {
        let Some(stack) = self.outputs.get(&output) else { return vec![] };
        stack
            .iter()
            .filter_map(|l| {
                let LayerContent::Overlay(kind) = &l.content else { return None };
                let opacity = l.opacity.value(now);
                let base = Overlay {
                    cue: l.cue,
                    z: l.z,
                    text: String::new(),
                    label: None,
                    font_size: 48.0,
                    color: [1.0; 4],
                    opacity,
                    scroll: 0.0,
                    mirror: false,
                    monospace: false,
                };
                Some(match kind {
                    OverlayKind::Text(t) => Overlay {
                        text: t.text.clone(),
                        font_size: t.font_size,
                        color: t.color,
                        scroll: t.scroll_speed * l.elapsed(now).as_secs_f32(),
                        mirror: t.mirror_horizontal,
                        ..base
                    },
                    OverlayKind::Timer(t) => {
                        let (text, warn) = timer_text(t, now);
                        Overlay {
                            text,
                            label: (!t.label.is_empty()).then(|| t.label.clone()),
                            font_size: t.font_size,
                            color: if warn { t.warn_color } else { t.color },
                            monospace: true,
                            ..base
                        }
                    }
                    OverlayKind::Notice(m) => {
                        Overlay { text: m.clone(), font_size: 36.0, color: [1.0, 0.76, 0.03, 1.0], ..base }
                    }
                })
            })
            .collect()
    }

    pub fn media_position(&self, cue: CueId, now: ShowTime) -> Option<ShowTime> {
        let out = self.cue_output.get(&cue)?;
        let l = self.outputs.get(out)?.iter().find(|l| l.cue == cue && !l.ghost)?;
        l.is_texture().then(|| l.elapsed(now))
    }

    /// Cues whose media reached its end (published to the show thread).
    pub fn finished_cues(&self) -> &std::collections::HashSet<CueId> {
        &self.finished
    }

}

/// Per-frame summary of frame-locked layers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SyncReport {
    /// Layers bound to a master.
    pub layers: u32,
    /// Of those, layers whose master clock was available.
    pub locked: u32,
    /// Largest |master − layer| this frame, seconds.
    pub worst_error: f64,
    /// Layers hard re-timed this frame.
    pub snaps: u32,
}

/// Formats a timer; returns `(text, in_warning_zone)`.
pub fn timer_text(t: &TimerContent, now: ShowTime) -> (String, bool) {
    let elapsed = (now - t.started_at) + t.elapsed_offset;
    match t.mode {
        TimerMode::CountUp => (elapsed.to_timer_string(), false),
        TimerMode::CountDown { from } => {
            let remaining = from - elapsed;
            // Round up so "00:01" is shown for the whole last second.
            let shown = ShowTime::from_micros(remaining.as_micros().div_ceil(1_000_000) * 1_000_000);
            (shown.to_timer_string(), remaining <= t.warn_below)
        }
        TimerMode::TimeOfDay => (chrono::Local::now().format("%H:%M:%S").to_string(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer(mode: TimerMode) -> TimerContent {
        TimerContent {
            mode,
            label: "Changeover".into(),
            font_size: 200.0,
            color: [1.0; 4],
            warn_color: [1.0, 0.0, 0.0, 1.0],
            warn_below: ShowTime::from_secs(30),
            started_at: ShowTime::from_secs(100),
            elapsed_offset: ShowTime::ZERO,
        }
    }

    #[test]
    fn countdown_text_and_warning() {
        let t = timer(TimerMode::CountDown { from: ShowTime::from_secs(300) });
        assert_eq!(timer_text(&t, ShowTime::from_secs(100)), ("05:00".into(), false));
        assert_eq!(timer_text(&t, ShowTime::from_millis(100_500)), ("05:00".into(), false));
        assert_eq!(timer_text(&t, ShowTime::from_secs(380)), ("00:20".into(), true));
        assert_eq!(timer_text(&t, ShowTime::from_secs(500)), ("00:00".into(), true));
    }

    #[test]
    fn show_fade_hide_lifecycle() {
        let mut c = Compositor::default();
        let out = OutputId::new();
        let cue = CueId::new();
        let none = |_: &AssetRef| None;
        c.handle(
            ShowTime::ZERO,
            &VideoAction::Show {
                cue,
                output: out,
                content: VisualContent::Color { rgba: [1.0, 0.0, 0.0, 1.0] },
                layer: 0,
                opacity: 1.0,
                fade_in: ShowTime::from_secs(2),
                start_at: ShowTime::ZERO,
                sync: None,
            },
            &none,
        );
        assert!((c.layers_mut(out)[0].opacity.value(ShowTime::from_secs(1)) - 0.5).abs() < 1e-6);
        c.handle(ShowTime::from_secs(2), &VideoAction::Hide { cue, fade: ShowTime::from_secs(1) }, &none);
        c.update(ShowTime::from_millis(2_500));
        assert!(!c.layers_mut(out).is_empty());
        c.update(ShowTime::from_secs(3));
        assert!(c.layers_mut(out).is_empty());
    }

    #[test]
    fn missing_asset_becomes_notice_overlay() {
        let mut c = Compositor::default();
        let out = OutputId::new();
        c.handle(
            ShowTime::ZERO,
            &VideoAction::Show {
                cue: CueId::new(),
                output: out,
                content: VisualContent::Video { asset: AssetRef::new("assets/video/x.mov"), looped: false },
                layer: 0,
                opacity: 1.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                sync: None,
            },
            &|_| None,
        );
        let o = c.overlays(out, ShowTime::ZERO);
        assert!(o[0].text.starts_with("MISSING ASSET"));
    }
}
