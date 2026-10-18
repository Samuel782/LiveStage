//! Projection mapping data and math: Bezier grid warp, edge blending and
//! polygonal masks. GPU-agnostic so it can be unit-tested and shared by the
//! renderer (`livestage_video`) and the editor UI.

use serde::{Deserialize, Serialize};

/// A tensor-product Bezier surface defined by a `cols x rows` control
/// lattice in normalised output space (0,0 = top-left, 1,1 = bottom-right).
///
/// A 4x4 lattice is a classic bicubic patch; larger lattices raise the
/// degree, giving smooth global warps with few handles. Because a Bezier
/// surface has *linear precision*, a uniformly spaced lattice produces the
/// identity mapping — the default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarpGrid {
    pub cols: usize,
    pub rows: usize,
    /// Row-major control points, `rows * cols` entries.
    pub points: Vec<[f32; 2]>,
    /// Tessellation density per axis used to build the render mesh.
    pub subdivisions: u32,
}

impl Default for WarpGrid {
    fn default() -> Self {
        Self::identity(4, 4)
    }
}

impl WarpGrid {
    pub fn identity(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(2), rows.max(2));
        let mut points = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            for c in 0..cols {
                points.push([c as f32 / (cols - 1) as f32, r as f32 / (rows - 1) as f32]);
            }
        }
        Self { cols, rows, points, subdivisions: 32 }
    }

    pub fn point(&self, col: usize, row: usize) -> [f32; 2] {
        self.points[row * self.cols + col]
    }
    pub fn point_mut(&mut self, col: usize, row: usize) -> &mut [f32; 2] {
        &mut self.points[row * self.cols + col]
    }

    pub fn is_valid(&self) -> bool {
        self.cols >= 2 && self.rows >= 2 && self.points.len() == self.cols * self.rows
    }

    /// Evaluates the surface at parametric `(u, v)` in `[0,1]^2` using
    /// de Casteljau's algorithm along each row, then along the column of
    /// intermediate results (numerically stable for any degree).
    pub fn evaluate(&self, u: f32, v: f32) -> [f32; 2] {
        debug_assert!(self.is_valid());
        let mut column: Vec<[f32; 2]> = Vec::with_capacity(self.rows);
        let mut scratch: Vec<[f32; 2]> = Vec::with_capacity(self.cols);
        for r in 0..self.rows {
            scratch.clear();
            scratch.extend_from_slice(&self.points[r * self.cols..(r + 1) * self.cols]);
            column.push(de_casteljau(&mut scratch, u));
        }
        de_casteljau(&mut column, v)
    }

    /// Tessellates the warp into a triangle mesh: each vertex carries its
    /// warped output position and the canvas UV it samples.
    pub fn tessellate(&self) -> WarpMesh {
        let n = self.subdivisions.max(1);
        let mut vertices = Vec::with_capacity(((n + 1) * (n + 1)) as usize);
        for j in 0..=n {
            for i in 0..=n {
                let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
                vertices.push(WarpVertex { position: self.evaluate(u, v), uv: [u, v] });
            }
        }
        let stride = n + 1;
        let mut indices = Vec::with_capacity((n * n * 6) as usize);
        for j in 0..n {
            for i in 0..n {
                let a = j * stride + i;
                let b = a + 1;
                let c = a + stride;
                let d = c + 1;
                indices.extend_from_slice(&[a, c, b, b, c, d]);
            }
        }
        WarpMesh { vertices, indices }
    }
}

fn de_casteljau(pts: &mut [[f32; 2]], t: f32) -> [f32; 2] {
    let n = pts.len();
    for k in 1..n {
        for i in 0..n - k {
            pts[i][0] += (pts[i + 1][0] - pts[i][0]) * t;
            pts[i][1] += (pts[i + 1][1] - pts[i][1]) * t;
        }
    }
    pts[0]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpVertex {
    /// Normalised output position (0..1, y down).
    pub position: [f32; 2],
    /// Canvas texture coordinate.
    pub uv: [f32; 2],
}

#[derive(Clone, Debug, Default)]
pub struct WarpMesh {
    pub vertices: Vec<WarpVertex>,
    pub indices: Vec<u32>,
}

/// Soft-edge ramp on one side of a projector overlap.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendEdge {
    /// Width of the ramp as a fraction of the canvas (0 = disabled).
    pub width: f32,
    /// Curve exponent shaping the ramp (≈2 for S-curve blends).
    pub power: f32,
    /// Display gamma compensation (typically 2.2).
    pub gamma: f32,
}

impl Default for BlendEdge {
    fn default() -> Self {
        Self { width: 0.0, power: 2.0, gamma: 2.2 }
    }
}

impl BlendEdge {
    /// Attenuation factor at distance `d` (0..1 canvas units) from the edge.
    pub fn factor(&self, d: f32) -> f32 {
        if self.width <= 0.0 {
            return 1.0;
        }
        let x = (d / self.width).clamp(0.0, 1.0);
        // Symmetric power curve so two overlapping ramps sum to 1 in linear light.
        let curve = if x < 0.5 {
            0.5 * (2.0 * x).powf(self.power)
        } else {
            1.0 - 0.5 * (2.0 * (1.0 - x)).powf(self.power)
        };
        curve.powf(1.0 / self.gamma.max(0.01))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgeBlend {
    pub left: BlendEdge,
    pub right: BlendEdge,
    pub top: BlendEdge,
    pub bottom: BlendEdge,
}

/// Polygonal mask in normalised output space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolygonMask {
    pub name: String,
    pub points: Vec<[f32; 2]>,
    /// false = hide inside the polygon, true = show only inside.
    pub invert: bool,
    pub enabled: bool,
}

impl PolygonMask {
    /// Even-odd point-in-polygon test.
    pub fn contains(&self, p: [f32; 2]) -> bool {
        let n = self.points.len();
        if n < 3 {
            return false;
        }
        let mut inside = false;
        let mut j = n - 1;
        for i in 0..n {
            let (pi, pj) = (self.points[i], self.points[j]);
            if (pi[1] > p[1]) != (pj[1] > p[1]) {
                let x = (pj[0] - pi[0]) * (p[1] - pi[1]) / (pj[1] - pi[1]) + pi[0];
                if p[0] < x {
                    inside = !inside;
                }
            }
            j = i;
        }
        inside
    }
}

/// Rasterises a set of masks into an 8-bit coverage image (255 = visible),
/// supersampled 2x2 per pixel for anti-aliased edges.
pub fn rasterize_masks(masks: &[PolygonMask], width: u32, height: u32) -> Vec<u8> {
    let active: Vec<&PolygonMask> = masks.iter().filter(|m| m.enabled && m.points.len() >= 3).collect();
    let mut out = vec![255u8; (width * height) as usize];
    if active.is_empty() {
        return out;
    }
    const OFFS: [[f32; 2]; 4] = [[0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]];
    for y in 0..height {
        for x in 0..width {
            let mut hits = 0u32;
            for o in OFFS {
                let p = [(x as f32 + o[0]) / width as f32, (y as f32 + o[1]) / height as f32];
                let visible = active.iter().all(|m| m.contains(p) == m.invert);
                hits += visible as u32;
            }
            out[(y * width + x) as usize] = (hits * 255 / 4) as u8;
        }
    }
    out
}

/// Everything needed to map one logical display onto a physical surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectionSetup {
    pub warp: WarpGrid,
    pub blend: EdgeBlend,
    pub masks: Vec<PolygonMask>,
    /// Output brightness multiplier.
    #[serde(default = "one")]
    pub brightness: f32,
}
fn one() -> f32 {
    1.0
}

impl Default for ProjectionSetup {
    /// Identity mapping at full brightness. (Deriving `Default` here once
    /// produced brightness 0.0, i.e. black program outputs.)
    fn default() -> Self {
        Self { warp: WarpGrid::default(), blend: EdgeBlend::default(), masks: Vec::new(), brightness: 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_lattice_is_identity_map() {
        let g = WarpGrid::identity(5, 4);
        for &(u, v) in &[(0.0, 0.0), (0.3, 0.7), (1.0, 1.0), (0.5, 0.5)] {
            let p = g.evaluate(u, v);
            assert!((p[0] - u).abs() < 1e-5 && (p[1] - v).abs() < 1e-5, "{p:?} vs {u},{v}");
        }
    }

    #[test]
    fn corners_are_interpolated() {
        let mut g = WarpGrid::identity(4, 4);
        *g.point_mut(3, 3) = [0.9, 0.8];
        assert_eq!(g.evaluate(1.0, 1.0), [0.9, 0.8]);
        let m = g.tessellate();
        assert_eq!(m.vertices.len(), 33 * 33);
        assert_eq!(m.indices.len(), 32 * 32 * 6);
    }

    #[test]
    fn blend_ramps() {
        let e = BlendEdge { width: 0.2, power: 2.0, gamma: 1.0 };
        assert_eq!(e.factor(0.0), 0.0);
        assert!((e.factor(0.1) - 0.5).abs() < 1e-6);
        assert_eq!(e.factor(0.5), 1.0);
        // Complementary ramps sum to one in linear light.
        assert!((e.factor(0.05) + e.factor(0.15) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn default_setup_is_visible() {
        assert_eq!(ProjectionSetup::default().brightness, 1.0);
    }

    #[test]
    fn mask_coverage() {
        let m = PolygonMask {
            name: "left half".into(),
            points: vec![[0.0, 0.0], [0.5, 0.0], [0.5, 1.0], [0.0, 1.0]],
            invert: false,
            enabled: true,
        };
        let img = rasterize_masks(&[m], 4, 1);
        assert_eq!(img, vec![0, 0, 255, 255]);
    }
}
