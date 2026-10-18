//! Strongly typed identifiers. All ids are UUIDv4 so that cues, outputs and
//! assets can be copied between shows without collisions.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self { Self(Uuid::new_v4()) }
            pub const fn nil() -> Self { Self(Uuid::nil()) }
        }
        impl Default for $name {
            fn default() -> Self { Self::new() }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // Short form keeps logs readable.
                write!(f, "{}({})", stringify!($name), &self.0.simple().to_string()[..8])
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(&self.0, f) }
        }
    };
}

define_id!(/// Identifies a cue anywhere in the show tree.
    CueId);
define_id!(/// Identifies a cue list.
    CueListId);
define_id!(/// Identifies a cue cart (soundboard page).
    CartId);
define_id!(/// Identifies a logical (named) video output, e.g. "Main LED Wall".
    OutputId);
define_id!(/// Identifies a logical audio stem (Vocals, Click, Backing...).
    StemId);
define_id!(/// Identifies a physical-output bus preset (FOH Main, In-Ears...).
    BusId);
define_id!(/// Identifies an imported asset.
    AssetId);
define_id!(/// Identifies a patched DMX fixture.
    FixtureId);
define_id!(/// Identifies a project.
    ProjectId);
