//! # livestage_core
//!
//! The engine-agnostic heart of LiveStage:
//!
//! * [`cue`] — the [`Cue`](cue::Cue) trait, the concrete cue types and the
//!   lifecycle state machine every cue goes through.
//! * [`timeline`] — stacked / timeline cues: a master cue acting as clock
//!   backbone with child cues fired at precise offsets.
//! * [`cuelist`] — ordered cue lists with playhead, standby, auto-continue /
//!   auto-follow and panic handling.
//! * [`routing`] — logical displays, audio stems/buses/patch crosspoints.
//! * [`projection`] — serialisable projection-mapping data (Bezier warp grid,
//!   edge blend, polygon masks) and the pure math to evaluate it.
//! * [`project`] / [`bundle`] — the `.livestage` folder bundle format.
//!
//! Nothing in this crate talks to hardware. Cues emit [`EngineAction`]s
//! through a [`CueContext`]; the application wires those actions to the
//! audio / video / DMX / control engines.

pub mod action;
pub mod bundle;
pub mod command;
pub mod cue;
pub mod cuelist;
pub mod dmx_config;
pub mod error;
pub mod id;
pub mod project;
pub mod projection;
pub mod routing;
pub mod state;
pub mod time;
pub mod timeline;

pub use action::{AudioAction, ControlAction, DmxAction, EngineAction, VideoAction};
pub use bundle::ProjectBundle;
pub use command::ShowCommand;
pub use cue::{AnyCue, Cue, CueContext, CueExt, CueHeader, CueKind};
pub use cuelist::CueList;
pub use error::{CoreError, CoreResult};
pub use id::*;
pub use project::Project;
pub use state::{CueState, Rgb};
pub use time::ShowTime;
pub use timeline::{TimelineCue, TimelineItem};
