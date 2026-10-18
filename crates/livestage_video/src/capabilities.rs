//! What movie playback can do on this machine, probed once at startup so
//! the operator learns about a missing decoder before the show, not when a
//! cue fails to appear.

use std::sync::OnceLock;

/// Hardware video decode status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HwDecode {
    /// The platform decode device opened.
    Available { api: &'static str },
    /// The platform API exists but its device could not be opened, so movies
    /// decode in software.
    Unavailable { api: &'static str, reason: String },
    /// Turned off with `LIVESTAGE_HWDEC=0` (also the Settings switch).
    Disabled,
    /// This OS has no hardware decode path in LiveStage.
    Unsupported,
}

/// Movie-file playback status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovieSupport {
    /// Built without the `ffmpeg` cargo feature.
    NotBuilt,
    /// FFmpeg is linked but failed to initialise.
    InitFailed(String),
    Ready {
        /// FFmpeg release string (`av_version_info`).
        version: String,
        hardware: HwDecode,
        /// Essential show codecs this FFmpeg build cannot decode.
        missing: Vec<&'static str>,
        /// Less common codecs this build cannot decode.
        missing_optional: Vec<&'static str>,
    },
}

/// Decoders every show machine needs: FFmpeg name, label.
pub const ESSENTIAL_DECODERS: &[(&str, &str)] = &[("h264", "H.264"), ("hevc", "HEVC / H.265"), ("prores", "ProRes")];
/// Decoders that are nice to have.
pub const OPTIONAL_DECODERS: &[(&str, &str)] = &[
    ("hap", "HAP"),
    ("dnxhd", "DNxHD / DNxHR"),
    ("vp9", "VP9"),
    ("av1", "AV1"),
    ("mpeg2video", "MPEG-2"),
    ("mjpeg", "Motion JPEG"),
];

impl MovieSupport {
    /// Whether movie files can play at all.
    pub fn movies_available(&self) -> bool {
        matches!(self, MovieSupport::Ready { .. })
    }

    /// One line for logs and Settings.
    pub fn summary(&self) -> String {
        match self {
            MovieSupport::NotBuilt => "Movie playback not built in (stills and image sequences only)".into(),
            MovieSupport::InitFailed(e) => format!("FFmpeg failed to initialise: {e}"),
            MovieSupport::Ready { version, hardware, .. } => {
                let hw = match hardware {
                    HwDecode::Available { api } => format!("{api} hardware decode"),
                    HwDecode::Unavailable { api, .. } => format!("software decode ({api} unavailable)"),
                    HwDecode::Disabled => "software decode (hardware decode off)".into(),
                    HwDecode::Unsupported => "software decode".into(),
                };
                format!("FFmpeg {version}, {hw}")
            }
        }
    }

    /// What the operator should be warned about at startup, if anything.
    /// Hardware decode switched off on purpose is not a warning.
    pub fn warning(&self) -> Option<String> {
        match self {
            MovieSupport::NotBuilt => Some(
                "Movie playback is not available: this build has no FFmpeg. Movie cues will not play; stills and image sequences still work."
                    .into(),
            ),
            MovieSupport::InitFailed(e) => Some(format!("Movie playback is not available: FFmpeg failed to initialise ({e}).")),
            MovieSupport::Ready { hardware, missing, .. } => {
                let mut parts = Vec::new();
                if !missing.is_empty() {
                    parts.push(format!("FFmpeg cannot decode {}; those movies will not play.", missing.join(", ")));
                }
                if let HwDecode::Unavailable { api, reason } = hardware {
                    parts.push(format!("{api} hardware decode is unavailable ({reason}); movies decode on the CPU."));
                }
                (!parts.is_empty()).then(|| parts.join(" "))
            }
        }
    }
}

/// Probes movie playback once per process; later calls return the same result.
/// Call after preferences have set `LIVESTAGE_HWDEC`.
pub fn movie_support() -> &'static MovieSupport {
    static PROBE: OnceLock<MovieSupport> = OnceLock::new();
    PROBE.get_or_init(probe)
}

#[cfg(feature = "ffmpeg")]
fn probe() -> MovieSupport {
    crate::ffmpeg_backend::probe()
}

#[cfg(not(feature = "ffmpeg"))]
fn probe() -> MovieSupport {
    MovieSupport::NotBuilt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(hardware: HwDecode, missing: Vec<&'static str>) -> MovieSupport {
        MovieSupport::Ready { version: "9.0".into(), hardware, missing, missing_optional: vec!["HAP"] }
    }

    #[test]
    fn warnings() {
        assert!(MovieSupport::NotBuilt.warning().is_some());
        assert!(MovieSupport::InitFailed("x".into()).warning().is_some());
        assert_eq!(ready(HwDecode::Available { api: "VideoToolbox" }, vec![]).warning(), None);
        // Turning hardware decode off is the operator's choice, and optional codecs don't warn.
        assert_eq!(ready(HwDecode::Disabled, vec![]).warning(), None);
        let w = ready(HwDecode::Unavailable { api: "VAAPI", reason: "no device".into() }, vec!["ProRes"]).warning().unwrap();
        assert!(w.contains("ProRes") && w.contains("VAAPI"), "{w}");
    }

    #[test]
    fn probe_matches_build() {
        let s = movie_support();
        assert_eq!(s.movies_available(), cfg!(feature = "ffmpeg"), "{}", s.summary());
    }
}
