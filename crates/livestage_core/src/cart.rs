//! Cue carts: soundboard pages of pads for live "busking".
//!
//! A [`CueCart`] is a grid of [`CartPad`]s. Each pad wraps an ordinary cue
//! (audio stinger, one-shot video, DMX flash look, or anything else), so pads
//! play through the same engines, fades and routing as list cues and can
//! be edited in the Inspector. What a pad does when pressed and released
//! is its [`PadMode`]:
//!
//! | Mode       | Press                          | Release            |
//! |------------|--------------------------------|--------------------|
//! | Toggle     | start, or stop if playing      | —                  |
//! | Retrigger  | (re)start from the top         | —                  |
//! | Hold       | start                          | stop (cue's fade)  |
//!
//! Pads run independently of cue lists: GO and the playhead never touch
//! them, but Stop All, Pause / Resume and Panic do.

use crate::cue::{AnyCue, Cue, CueContext, CueEvent, CueExt};
use crate::id::{CartId, CueId};
use crate::state::Rgb;
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};

/// Largest grid side.
pub const MAX_SIDE: u8 = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadMode {
    /// Press starts; pressing again stops (with the cue's fade-out).
    #[default]
    Toggle,
    /// Every press restarts from the beginning.
    Retrigger,
    /// Plays while held (gate); release stops with the cue's fade-out.
    Hold,
}

impl PadMode {
    pub const ALL: [PadMode; 3] = [PadMode::Toggle, PadMode::Retrigger, PadMode::Hold];
    pub fn label(self) -> &'static str {
        match self {
            PadMode::Toggle => "Toggle",
            PadMode::Retrigger => "Retrigger",
            PadMode::Hold => "Hold / Gate",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            PadMode::Toggle => "TGL",
            PadMode::Retrigger => "RTG",
            PadMode::Hold => "HOLD",
        }
    }
}

/// A MIDI note that plays a pad (note-on presses, note-off releases).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PadNote {
    /// 0-based channel.
    pub channel: u8,
    pub note: u8,
}

impl std::fmt::Display for PadNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        let octave = self.note as i32 / 12 - 1;
        write!(f, "{}{} ch{}", NAMES[(self.note % 12) as usize], octave, self.channel + 1)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CartPad {
    pub cue: AnyCue,
    #[serde(default)]
    pub mode: PadMode,
    /// Pad colour (None = by cue kind).
    #[serde(default)]
    pub color: Option<Rgb>,
    /// Keyboard key (egui key name, e.g. `"Q"`, `"Num1"`, `"F5"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<PadNote>,
    /// Held down right now (Hold mode bookkeeping).
    #[serde(skip)]
    pub held: bool,
}

impl CartPad {
    pub fn new(cue: AnyCue) -> Self {
        Self { cue, mode: PadMode::Toggle, color: None, key: None, midi: None, held: false }
    }

    /// Press. Returns true if the pad started playing.
    pub fn press(&mut self, ctx: &mut dyn CueContext) -> bool {
        let active = self.is_playing();
        match self.mode {
            PadMode::Toggle if active => {
                self.cue.stop(ctx, None);
                false
            }
            PadMode::Hold => {
                self.held = true;
                if !active {
                    self.cue.go(ctx);
                }
                true
            }
            // Toggle (idle) and Retrigger: `go` restarts a playing cue from the top.
            _ => {
                self.cue.go(ctx);
                true
            }
        }
    }

    /// Release. Only Hold pads react (stop with the cue's own fade-out).
    pub fn release(&mut self, ctx: &mut dyn CueContext) {
        if self.mode == PadMode::Hold && self.held {
            self.held = false;
            self.cue.stop(ctx, None);
        }
    }

    /// The pad is on: its cue is running, a DMX look it applied is still
    /// up (its fade finished, but the look holds until released), or it is
    /// a Hold pad being held.
    pub fn is_playing(&self) -> bool {
        self.cue.state().is_active() || self.latched() || (self.mode == PadMode::Hold && self.held)
    }

    /// A completed DMX look that still holds the rig until stopped.
    pub fn latched(&self) -> bool {
        self.cue.state() == crate::state::CueState::Completed && matches!(&self.cue, AnyCue::Dmx(d) if d.release_on_stop)
    }
}

/// A soundboard page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueCart {
    pub id: CartId,
    pub name: String,
    pub rows: u8,
    pub cols: u8,
    /// Row-major, `rows × cols` slots.
    pub pads: Vec<Option<CartPad>>,
}

impl CueCart {
    pub fn new(name: impl Into<String>, rows: u8, cols: u8) -> Self {
        let (rows, cols) = (rows.clamp(1, MAX_SIDE), cols.clamp(1, MAX_SIDE));
        Self { id: CartId::new(), name: name.into(), rows, cols, pads: vec![None; rows as usize * cols as usize] }
    }

    pub fn len(&self) -> usize {
        self.rows as usize * self.cols as usize
    }

    pub fn is_empty(&self) -> bool {
        self.pads.iter().all(Option::is_none)
    }

    /// Changes the grid, keeping every pad at its row / column when it still
    /// fits and moving the others into free slots (nothing is dropped while
    /// there is room). Returns pads that no longer fit.
    pub fn resize(&mut self, rows: u8, cols: u8) -> Vec<CartPad> {
        let (rows, cols) = (rows.clamp(1, MAX_SIDE), cols.clamp(1, MAX_SIDE));
        let old_cols = self.cols as usize;
        let mut grid: Vec<Option<CartPad>> = vec![None; rows as usize * cols as usize];
        let mut homeless = Vec::new();
        for (i, pad) in std::mem::take(&mut self.pads).into_iter().enumerate() {
            let Some(pad) = pad else { continue };
            let (r, c) = (i / old_cols, i % old_cols);
            if r < rows as usize && c < cols as usize {
                grid[r * cols as usize + c] = Some(pad);
            } else {
                homeless.push(pad);
            }
        }
        let mut overflow = Vec::new();
        for pad in homeless {
            match grid.iter_mut().find(|s| s.is_none()) {
                Some(slot) => *slot = Some(pad),
                None => overflow.push(pad),
            }
        }
        self.rows = rows;
        self.cols = cols;
        self.pads = grid;
        overflow
    }

    pub fn pad(&self, slot: usize) -> Option<&CartPad> {
        self.pads.get(slot)?.as_ref()
    }
    pub fn pad_mut(&mut self, slot: usize) -> Option<&mut CartPad> {
        self.pads.get_mut(slot)?.as_mut()
    }

    pub fn slot_of(&self, cue: CueId) -> Option<usize> {
        self.pads.iter().position(|p| p.as_ref().is_some_and(|p| p.cue.header().id == cue))
    }

    pub fn press(&mut self, ctx: &mut dyn CueContext, slot: usize) -> bool {
        self.pad_mut(slot).is_some_and(|p| p.press(ctx))
    }

    pub fn release(&mut self, ctx: &mut dyn CueContext, slot: usize) {
        if let Some(p) = self.pad_mut(slot) {
            p.release(ctx);
        }
    }

    /// Advances playing pads. Call once per show tick.
    pub fn tick(&mut self, ctx: &mut dyn CueContext) {
        for p in self.pads.iter_mut().flatten() {
            // A held pad stays held when its cue completes: the release still
            // has to stop it (e.g. release a DMX flash whose fade is over).
            if p.cue.state().is_active() && p.cue.tick(ctx) == Some(CueEvent::Completed) && p.mode != PadMode::Hold {
                p.held = false;
            }
        }
    }

    pub fn stop_all(&mut self, ctx: &mut dyn CueContext, fade: Option<ShowTime>) {
        for p in self.pads.iter_mut().flatten() {
            p.held = false;
            p.cue.stop(ctx, fade);
        }
    }

    pub fn pause_all(&mut self, ctx: &mut dyn CueContext) {
        for p in self.pads.iter_mut().flatten() {
            p.cue.pause(ctx);
        }
    }

    pub fn resume_all(&mut self, ctx: &mut dyn CueContext) {
        for p in self.pads.iter_mut().flatten() {
            p.cue.resume(ctx);
        }
    }

    pub fn playing(&self) -> usize {
        self.pads.iter().flatten().filter(|p| p.is_playing()).count()
    }

    pub fn find(&self, id: CueId) -> Option<&AnyCue> {
        self.pads.iter().flatten().find_map(|p| p.cue.find(id))
    }

    /// Copies live pad state from `old` (undo / redo), matching by cue id.
    pub fn adopt_runtime_from(&mut self, old: &CueCart) {
        for p in self.pads.iter_mut().flatten() {
            let id = p.cue.header().id;
            if let Some(op) = old.pads.iter().flatten().find(|o| o.cue.header().id == id) {
                p.cue.header_mut().runtime = op.cue.header().runtime.clone();
                p.held = op.held;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{AudioAction, AudioTrack, DmxAction, EngineAction};
    use crate::cue::test_support::*;
    use crate::cue::{AudioCue, CueHeader};
    use crate::id::StemId;
    use crate::project::AssetRef;
    use crate::state::CueState;

    fn stinger() -> AnyCue {
        AnyCue::Audio(AudioCue {
            header: CueHeader::new("P1", "Stinger"),
            tracks: vec![AudioTrack { asset: AssetRef::new("assets/audio/hit.wav"), stem: StemId::new(), gain_db: 0.0, mute: false }],
            click: None,
            gain_db: 0.0,
            fade_in: ShowTime::ZERO,
            fade_out: ShowTime::from_millis(300),
            start_at: ShowTime::ZERO,
            end_at: None,
            looped: false,
            media_length: Some(ShowTime::from_secs(4)),
        })
    }

    fn plays(ctx: &MockCtx) -> usize {
        ctx.actions.iter().filter(|a| matches!(a, EngineAction::Audio(AudioAction::Play { .. }))).count()
    }
    fn stops(ctx: &MockCtx) -> Vec<ShowTime> {
        ctx.actions
            .iter()
            .filter_map(|a| match a {
                EngineAction::Audio(AudioAction::Stop { fade, .. }) => Some(*fade),
                _ => None,
            })
            .collect()
    }

    fn cart_with(mode: PadMode, cue: AnyCue) -> CueCart {
        let mut c = CueCart::new("Stingers", 4, 4);
        let mut pad = CartPad::new(cue);
        pad.mode = mode;
        c.pads[5] = Some(pad);
        c
    }

    #[test]
    fn toggle_starts_then_stops_with_fade() {
        let mut ctx = MockCtx::default();
        let mut c = cart_with(PadMode::Toggle, stinger());
        assert!(c.press(&mut ctx, 5));
        assert_eq!(plays(&ctx), 1);
        assert!(c.pad(5).unwrap().is_playing());
        c.release(&mut ctx, 5);
        assert!(c.pad(5).unwrap().is_playing(), "release does nothing in toggle mode");
        ctx.advance(ShowTime::from_secs(1));
        assert!(!c.press(&mut ctx, 5));
        assert_eq!(stops(&ctx), vec![ShowTime::from_millis(300)], "cue's own fade-out");
        assert_eq!(c.pad(5).unwrap().cue.state(), CueState::Stopped);
        // And again from the top.
        assert!(c.press(&mut ctx, 5));
        assert_eq!(plays(&ctx), 2);
    }

    #[test]
    fn retrigger_restarts_from_the_beginning() {
        let mut ctx = MockCtx::default();
        let mut c = cart_with(PadMode::Retrigger, stinger());
        c.press(&mut ctx, 5);
        ctx.advance(ShowTime::from_secs(2));
        c.tick(&mut ctx);
        assert_eq!(c.pad(5).unwrap().cue.elapsed(&ctx), ShowTime::from_secs(2));
        assert!(c.press(&mut ctx, 5));
        assert_eq!(plays(&ctx), 2);
        assert_eq!(stops(&ctx), vec![ShowTime::ZERO], "the old voice is cut, not faded");
        assert_eq!(c.pad(5).unwrap().cue.elapsed(&ctx), ShowTime::ZERO);
        assert!(c.pad(5).unwrap().is_playing());
    }

    #[test]
    fn hold_plays_only_while_held() {
        let mut ctx = MockCtx::default();
        let mut c = cart_with(PadMode::Hold, dmx_cue("F1", 0));
        c.press(&mut ctx, 5);
        assert!(ctx.actions.iter().any(|a| matches!(a, EngineAction::Dmx(DmxAction::ApplyScene { .. }))));
        assert!(c.pad(5).unwrap().held);
        // Key repeat / a second press while held does not restart the flash.
        let n = ctx.actions.len();
        c.press(&mut ctx, 5);
        assert_eq!(ctx.actions.len(), n);
        // The flash's fade is over but it holds the rig until released.
        ctx.advance(ShowTime::from_secs(1));
        c.tick(&mut ctx);
        assert_eq!(c.pad(5).unwrap().cue.state(), CueState::Completed);
        assert!(c.pad(5).unwrap().is_playing());
        c.release(&mut ctx, 5);
        assert!(ctx.actions.iter().any(|a| matches!(a, EngineAction::Dmx(DmxAction::Release { .. }))), "look released");
        assert!(!c.pad(5).unwrap().is_playing());
        assert!(!c.pad(5).unwrap().held);
        // A release without a press is ignored.
        let n = ctx.actions.len();
        c.release(&mut ctx, 5);
        assert_eq!(ctx.actions.len(), n);
    }

    #[test]
    fn pads_complete_and_stop_all() {
        let mut ctx = MockCtx::default();
        let mut c = cart_with(PadMode::Toggle, stinger());
        c.pads[0] = Some(CartPad::new(dmx_cue("F2", 0)));
        c.press(&mut ctx, 5);
        c.press(&mut ctx, 0);
        assert_eq!(c.playing(), 2);
        ctx.advance(ShowTime::from_secs(5));
        c.tick(&mut ctx);
        assert_eq!(c.pad(5).unwrap().cue.state(), CueState::Completed, "stinger ran out");
        assert!(!c.pad(5).unwrap().is_playing(), "finished audio is off");
        assert!(c.pad(0).unwrap().is_playing(), "a DMX look stays on until toggled off");
        assert!(!c.press(&mut ctx, 0), "toggle releases the look");
        assert!(!c.pad(0).unwrap().is_playing());
        c.press(&mut ctx, 0);
        c.stop_all(&mut ctx, Some(ShowTime::ZERO));
        assert_eq!(c.playing(), 0);
        // Pressing an empty slot is harmless.
        assert!(!c.press(&mut ctx, 15));
        assert!(!c.press(&mut ctx, 99));
    }

    #[test]
    fn resize_keeps_positions_and_rehomes() {
        let mut c = CueCart::new("A", 4, 4);
        c.pads[1] = Some(CartPad::new(dmx_cue("a", 0))); // r0 c1
        c.pads[15] = Some(CartPad::new(dmx_cue("b", 0))); // r3 c3
        let lost = c.resize(2, 2);
        assert!(lost.is_empty());
        assert_eq!(c.len(), 4);
        assert_eq!(c.pad(1).unwrap().cue.header().number, "a", "kept its row / column");
        assert_eq!(c.pad(0).unwrap().cue.header().number, "b", "moved to the first free slot");
        let lost = c.resize(1, 1);
        assert_eq!(lost.len(), 1);
        c.resize(8, 8);
        assert_eq!(c.pads.len(), 64);
    }

    #[test]
    fn serde_roundtrip_and_note_names() {
        let mut c = cart_with(PadMode::Hold, stinger());
        let p = c.pad_mut(5).unwrap();
        p.key = Some("Num1".into());
        p.midi = Some(PadNote { channel: 9, note: 36 });
        p.color = Some(Rgb(200, 40, 40));
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"mode\":\"hold\""));
        let back: CueCart = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        assert_eq!(PadNote { channel: 9, note: 36 }.to_string(), "C2 ch10");
        assert_eq!(PadNote { channel: 0, note: 60 }.to_string(), "C4 ch1");
    }
}
