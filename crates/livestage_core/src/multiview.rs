//! Multiview (control-booth monitor wall) layout, saved with the show.

use crate::id::OutputId;
use crate::routing::LogicalDisplay;
use serde::{Deserialize, Serialize};

/// What a multiview cell shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MultiviewSource {
    #[default]
    Empty,
    /// Live canvas of a logical output.
    Output { id: OutputId },
    /// The most recently started running cue: clocks, progress, picture.
    Program,
    /// The standby cue (what the next GO fires) with a content preview.
    Next,
    /// The running presentation: current and upcoming slide, text and notes.
    NextSlide,
    /// Time of day.
    Clock,
}

/// Grid of cells; optionally the first cell spans 2×2 ("featured").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiviewLayout {
    pub rows: u8,
    pub cols: u8,
    #[serde(default)]
    pub feature_first: bool,
    pub cells: Vec<MultiviewSource>,
}

impl MultiviewLayout {
    pub const MAX: u8 = 6;

    /// Number of cells the grid has.
    pub fn cell_count(&self) -> usize {
        let n = self.rows.max(1) as usize * self.cols.max(1) as usize;
        if self.has_feature() {
            n - 3
        } else {
            n
        }
    }

    pub fn has_feature(&self) -> bool {
        self.feature_first && self.rows >= 2 && self.cols >= 2
    }

    /// Resizes the grid, keeping existing assignments in order.
    pub fn set_grid(&mut self, rows: u8, cols: u8, feature_first: bool) {
        self.rows = rows.clamp(1, Self::MAX);
        self.cols = cols.clamp(1, Self::MAX);
        self.feature_first = feature_first;
        self.cells.resize(self.cell_count(), MultiviewSource::Empty);
    }

    /// Cell rectangles as `(col, row, col_span, row_span)`, in cell order.
    pub fn placements(&self) -> Vec<(u8, u8, u8, u8)> {
        let (rows, cols) = (self.rows.max(1), self.cols.max(1));
        let mut out = Vec::with_capacity(self.cell_count());
        if self.has_feature() {
            out.push((0, 0, 2, 2));
        }
        for r in 0..rows {
            for c in 0..cols {
                if self.has_feature() && r < 2 && c < 2 {
                    continue;
                }
                out.push((c, r, 1, 1));
            }
        }
        out
    }

    /// A sensible wall for a show: every output, then program / next / slides / clock.
    pub fn auto(displays: &[LogicalDisplay]) -> Self {
        let mut cells: Vec<MultiviewSource> =
            displays.iter().filter(|d| d.enabled).map(|d| MultiviewSource::Output { id: d.id }).collect();
        cells.extend([MultiviewSource::Program, MultiviewSource::Next, MultiviewSource::NextSlide, MultiviewSource::Clock]);
        let n = cells.len();
        let (rows, cols) = match n {
            0..=4 => (2, 2),
            5..=6 => (2, 3),
            7..=9 => (3, 3),
            10..=12 => (3, 4),
            _ => (4, 4),
        };
        let mut l = Self { rows, cols, feature_first: false, cells };
        l.cells.truncate(l.cell_count());
        l.set_grid(rows, cols, false);
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn featured_layout_spans_four_cells() {
        let mut l = MultiviewLayout { rows: 3, cols: 3, feature_first: true, cells: vec![] };
        l.set_grid(3, 3, true);
        assert_eq!(l.cell_count(), 6);
        let p = l.placements();
        assert_eq!(p[0], (0, 0, 2, 2));
        assert_eq!(&p[1..3], &[(2, 0, 1, 1), (2, 1, 1, 1)]);
        assert_eq!(p.len(), 6);
    }

    #[test]
    fn auto_layout_lists_outputs_then_booth_feeds() {
        let displays = vec![LogicalDisplay::new("LED"), LogicalDisplay::new("Confidence")];
        let l = MultiviewLayout::auto(&displays);
        assert_eq!((l.rows, l.cols), (2, 3));
        assert_eq!(l.cells[0], MultiviewSource::Output { id: displays[0].id });
        assert!(l.cells.contains(&MultiviewSource::Next));
        let json = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<MultiviewLayout>(&json).unwrap(), l);
    }
}
