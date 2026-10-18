//! Folding an editor's copy of the project back into the live project.
//!
//! The UI edits a copy while the show keeps running on the live project
//! (cue state, OSC faders, finished imports). [`ProjectEdits::diff`] does
//! every comparison against the copy the editor started from and needs no
//! lock; [`Project::apply_edits`] then only moves the changed pieces into the
//! live project, which keeps the time spent under the runtime lock small
//! even for very large shows.
//!
//! Rules: whatever the editor changed wins; whatever it left alone keeps the
//! live value. Cues are merged one by one (a running cue keeps playing while
//! another is edited, and an edited cue adopts the live runtime state), and
//! fader levels the editor did not move keep their live position.

use crate::cart::CueCart;
use crate::cue::{AnyCue, Cue};
use crate::cuelist::{CueList, CueSlot};
use crate::dmx_config::DmxConfig;
use crate::id::{CueId, CueListId, ProjectId, SubmasterId};
use crate::lighting::LightingConfig;
use crate::multiview::MultiviewLayout;
use crate::project::{AssetRecord, ControlConfig, Project};
use crate::routing::{AudioRouting, LogicalDisplay};
use crate::timecode::TimecodeConfig;
use std::collections::{HashMap, HashSet};

macro_rules! whole_fields {
    ($($f:ident: $t:ty),* $(,)?) => {
        /// Project fields replaced as a whole when edited.
        #[derive(Default)]
        struct WholeFields { $($f: Option<$t>,)* }
        impl WholeFields {
            fn is_empty(&self) -> bool {
                true $(&& self.$f.is_none())*
            }
            fn apply(self, p: &mut Project) {
                $(if let Some(v) = self.$f { p.$f = v; })*
            }
        }
    };
}

whole_fields!(
    schema_version: u32,
    id: ProjectId,
    name: String,
    created_at: u64,
    modified_at: u64,
    active_cue_list: Option<CueListId>,
    displays: Vec<LogicalDisplay>,
    dmx: DmxConfig,
    control: ControlConfig,
    assets: Vec<AssetRecord>,
    multiview: Option<MultiviewLayout>,
    timecode: TimecodeConfig,
);

/// One cue list in the editor's order.
enum ListEdit {
    /// Unchanged by the editor: the live list stays as it is.
    Keep(CueListId),
    /// Created by the editor.
    New(CueList),
    /// Changed: the editor's list-level fields (`shell`, cues emptied) and
    /// its cues, unchanged ones by id.
    Edit { shell: CueList, cues: Vec<CueSlot>, base_ids: HashSet<CueId> },
}

/// The editor's changes, ready to apply (see the module docs).
pub struct ProjectEdits {
    whole: WholeFields,
    audio: Option<AudioRouting>,
    gain_touched: bool,
    lighting: Option<LightingConfig>,
    grand_master_touched: bool,
    levels_touched: HashSet<SubmasterId>,
    lists: Option<(Vec<ListEdit>, HashSet<CueListId>)>,
    carts: Option<(Vec<CueCart>, Vec<CueCart>)>,
}

impl ProjectEdits {
    /// What the editor changed going from `base` to `ours`; None if nothing.
    pub fn diff(base: &Project, ours: Project) -> Option<Self> {
        let Project {
            schema_version,
            id,
            name,
            created_at,
            modified_at,
            cue_lists,
            active_cue_list,
            displays,
            audio,
            dmx,
            control,
            assets,
            multiview,
            timecode,
            carts,
            lighting,
        } = ours;
        macro_rules! changed {
            ($($f:ident),*) => { WholeFields { $($f: ($f != base.$f).then_some($f),)* } };
        }
        let whole = changed!(schema_version, id, name, created_at, modified_at, active_cue_list, displays, dmx, control, assets, multiview, timecode);
        let gain_touched = audio.master_gain_db != base.audio.master_gain_db;
        let audio = (audio != base.audio).then_some(audio);
        let grand_master_touched = lighting.grand_master != base.lighting.grand_master;
        let levels_touched = lighting
            .submasters
            .iter()
            .filter(|s| !base.lighting.submasters.iter().any(|b| b.id == s.id && b.level == s.level))
            .map(|s| s.id)
            .collect();
        let lighting = (lighting != base.lighting).then_some(lighting);
        let lists = (cue_lists != base.cue_lists).then(|| {
            let edits = cue_lists
                .into_iter()
                .map(|l| match base.cue_lists.iter().find(|b| b.id == l.id) {
                    Some(b) if *b == l => ListEdit::Keep(l.id),
                    Some(b) => {
                        let base_cues: HashMap<CueId, &AnyCue> = b.cues.iter().map(|c| (c.header().id, c)).collect();
                        let mut shell = l;
                        let cues = std::mem::take(&mut shell.cues)
                            .into_iter()
                            .map(|c| match base_cues.get(&c.header().id) {
                                Some(bc) if **bc == c => CueSlot::Keep(c.header().id),
                                _ => CueSlot::Take(Box::new(c)),
                            })
                            .collect();
                        ListEdit::Edit { shell, cues, base_ids: base_cues.keys().copied().collect() }
                    }
                    None => ListEdit::New(l),
                })
                .collect();
            (edits, base.cue_lists.iter().map(|l| l.id).collect())
        });
        let carts = (carts != base.carts).then(|| (carts, base.carts.clone()));
        let edits = Self { whole, audio, gain_touched, lighting, grand_master_touched, levels_touched, lists, carts };
        let empty = edits.whole.is_empty() && edits.audio.is_none() && edits.lighting.is_none() && edits.lists.is_none() && edits.carts.is_none();
        (!empty).then_some(edits)
    }
}

impl Project {
    /// Applies [`ProjectEdits`] to the live project: moves only, no
    /// whole-project comparisons or clones.
    pub fn apply_edits(&mut self, edits: ProjectEdits) {
        let ProjectEdits { whole, audio, gain_touched, lighting, grand_master_touched, levels_touched, lists, carts } = edits;
        whole.apply(self);
        if let Some(mut audio) = audio {
            if !gain_touched {
                audio.master_gain_db = self.audio.master_gain_db;
            }
            self.audio = audio;
        }
        if let Some(mut lighting) = lighting {
            if !grand_master_touched {
                lighting.grand_master = self.lighting.grand_master;
            }
            for s in &mut lighting.submasters {
                if let Some(live) = self.lighting.submasters.iter().find(|l| l.id == s.id).filter(|_| !levels_touched.contains(&s.id)) {
                    s.level = live.level;
                }
            }
            self.lighting = lighting;
        }
        if let Some((edits, base_ids)) = lists {
            let order: Vec<CueListId> = self.cue_lists.iter().map(|l| l.id).collect();
            let mut live: HashMap<CueListId, CueList> = self.cue_lists.drain(..).map(|l| (l.id, l)).collect();
            for e in edits {
                match e {
                    ListEdit::Keep(id) => self.cue_lists.extend(live.remove(&id)),
                    ListEdit::New(l) => self.cue_lists.push(l),
                    ListEdit::Edit { shell, cues, base_ids } => {
                        // Edited here, deleted live meanwhile: the deletion wins.
                        if let Some(l) = live.remove(&shell.id) {
                            self.cue_lists.push(CueList::rebuild_from_edit(shell, cues, &base_ids, l));
                        }
                    }
                }
            }
            // Lists the show added while the editor worked.
            for id in order.into_iter().filter(|id| !base_ids.contains(id)) {
                self.cue_lists.extend(live.remove(&id));
            }
        }
        if let Some((ours, base)) = carts {
            self.carts = merge_carts(ours, &base, std::mem::take(&mut self.carts));
        }
    }

    /// [`ProjectEdits::diff`] then [`apply_edits`](Self::apply_edits) in one step.
    pub fn merge_edits(&mut self, base: &Project, ours: Project) {
        if let Some(e) = ProjectEdits::diff(base, ours) {
            self.apply_edits(e);
        }
    }
}

/// Carts are small (≤ 64 pads), so they are merged pad by pad directly:
/// pads the editor left alone take the live pad, edited ones adopt the live
/// pad state.
fn merge_carts(ours: Vec<CueCart>, base: &[CueCart], mut live: Vec<CueCart>) -> Vec<CueCart> {
    let mut out = Vec::with_capacity(ours.len());
    for mut k in ours {
        let b = base.iter().find(|b| b.id == k.id);
        let Some(i) = live.iter().position(|l| l.id == k.id) else {
            // Deleted live while unchanged here, or created here.
            if b.is_none() || b != Some(&k) {
                out.push(k);
            }
            continue;
        };
        let live_k = live.remove(i);
        if b == Some(&k) {
            out.push(live_k);
            continue;
        }
        if let Some(b) = b {
            for (slot, pad) in k.pads.iter_mut().enumerate() {
                if b.pads.get(slot) == Some(&*pad) {
                    if let Some(lp) = live_k.pads.get(slot) {
                        pad.clone_from(lp);
                    }
                }
            }
        }
        k.adopt_runtime_from(&live_k);
        out.push(k);
    }
    // Carts the show added while the editor worked.
    out.extend(live.into_iter().filter(|l| !base.iter().any(|b| b.id == l.id)));
    out
}

#[cfg(test)]
mod tests {
    use crate::cue::test_support::*;
    use crate::cue::{Cue, CueExt};
    use crate::lighting::{Palette, PaletteKind, Submaster, SubmasterKind};
    use crate::state::CueState;
    use crate::time::ShowTime;
    use crate::Project;

    /// A show with two DMX cues and a submaster; cue 1 is running.
    fn running_show() -> (Project, MockCtx) {
        let mut ctx = MockCtx::default();
        let mut p = Project::new("Show");
        p.cue_lists[0].cues = vec![dmx_cue("1", 10_000), dmx_cue("2", 1_000)];
        p.lighting.submasters.push(Submaster::new("Wash", SubmasterKind::Inhibit, vec![]));
        p.cue_lists[0].reset(&mut ctx);
        p.cue_lists[0].go(&mut ctx);
        (p, ctx)
    }

    #[test]
    fn edit_keeps_live_cue_state() {
        let (mut live, mut ctx) = running_show();
        let base = live.clone();
        let mut ours = base.clone();
        ours.cue_lists[0].cues[1].header_mut().name = "Renamed".into();
        // The show moves on while the editor works.
        ctx.advance(ShowTime::from_millis(500));
        live.cue_lists[0].tick(&mut ctx);
        let live_cue1 = live.cue_lists[0].cues[0].clone();
        live.merge_edits(&base, ours);
        assert_eq!(live.cue_lists[0].cues[1].header().name, "Renamed");
        assert_eq!(live.cue_lists[0].cues[0], live_cue1, "untouched running cue is the live one");
        assert_eq!(live.cue_lists[0].cues[0].state(), CueState::Running);
        assert_eq!(live.cue_lists[0].standby_index(), Some(1));
    }

    #[test]
    fn edited_running_cue_adopts_runtime() {
        let (mut live, _) = running_show();
        let base = live.clone();
        let mut ours = base.clone();
        ours.cue_lists[0].cues[0].header_mut().name = "Edited while running".into();
        live.merge_edits(&base, ours);
        let c = &live.cue_lists[0].cues[0];
        assert_eq!(c.header().name, "Edited while running");
        assert_eq!(c.state(), CueState::Running);
    }

    #[test]
    fn reorder_keeps_standby_on_the_same_cue() {
        let (mut live, _) = running_show();
        let base = live.clone();
        let mut ours = base.clone();
        ours.cue_lists[0].cues.insert(0, dmx_cue("0.5", 0));
        live.merge_edits(&base, ours);
        let l = &live.cue_lists[0];
        assert_eq!(l.cues.len(), 3);
        assert_eq!(l.standby_cue().map(|c| c.header().number.clone()), Some("2".into()));
        assert_eq!(l.cues[1].state(), CueState::Running);
    }

    #[test]
    fn live_fader_and_import_survive_unrelated_edit() {
        let (mut live, _) = running_show();
        let base = live.clone();
        let mut ours = base.clone();
        ours.lighting.palettes.push(Palette::new("Red", PaletteKind::Color, vec![]));
        ours.audio.output_channels += 2;
        ours.cue_lists[0].cues[1].header_mut().name = "Edited".into();
        // OSC moves the faders and an import lands while the editor works.
        live.lighting.submasters[0].level = 0.4;
        live.lighting.grand_master = 0.7;
        live.audio.master_gain_db = -6.0;
        live.cue_lists[0].cues.push(dmx_cue("3", 0));
        live.merge_edits(&base, ours);
        assert_eq!(live.lighting.palettes.len(), 1);
        assert_eq!(live.lighting.submasters[0].level, 0.4);
        assert_eq!(live.lighting.grand_master, 0.7);
        assert_eq!(live.audio.master_gain_db, -6.0);
        assert_eq!(live.audio.output_channels, base.audio.output_channels + 2);
        let numbers: Vec<_> = live.cue_lists[0].cues.iter().map(|c| c.header().number.clone()).collect();
        assert_eq!(numbers, ["1", "2", "3"]);
    }

    #[test]
    fn editor_wins_on_what_it_changed() {
        let (mut live, _) = running_show();
        let base = live.clone();
        let mut ours = base.clone();
        ours.lighting.submasters[0].level = 0.9;
        ours.cue_lists[0].cues.remove(1);
        live.lighting.submasters[0].level = 0.2;
        live.merge_edits(&base, ours);
        assert_eq!(live.lighting.submasters[0].level, 0.9);
        assert_eq!(live.cue_lists[0].cues.len(), 1);
    }

    #[test]
    fn new_list_and_cart_are_added() {
        let (mut live, _) = running_show();
        let base = live.clone();
        let mut ours = base.clone();
        ours.cue_lists.push(crate::cuelist::CueList::new("B"));
        ours.carts.push(crate::cart::CueCart::new("Cart 1", 2, 2));
        live.merge_edits(&base, ours);
        assert_eq!(live.cue_lists.len(), 2);
        assert_eq!(live.carts.len(), 1);
    }

    #[test]
    fn no_edit_is_a_no_op() {
        let (mut live, _) = running_show();
        let base = live.clone();
        live.lighting.grand_master = 0.5;
        let expect = live.clone();
        assert!(super::ProjectEdits::diff(&base, base.clone()).is_none());
        live.merge_edits(&base, base.clone());
        assert_eq!(live, expect);
    }
}
