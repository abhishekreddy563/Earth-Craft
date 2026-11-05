//! Camera-matched geometry for backend analysis output.
use earthcraft_core::camera::Basis;
use emath::{Pos2, Rect};

const TILE_GRID_SEGMENTS: u32 = 12;
const OUTLINE_ARC_SEGMENTS: usize = 32;

pub struct TileQuad {
    pub positions: [Pos2; 4],
    pub texture_coordinates: [Pos2; 4],
}

pub fn project_xyz_tile(basis: &Basis, rect: Rect, z: u8, x: u32, y: u32) -> Vec<TileQuad> {
    let n = f64::from(1u32 << z);
    let project = |uv: Pos2| {
        let lon = (f64::from(x) + f64::from(uv.x)) / n * 360. - 180.;
        let lat = (std::f64::consts::PI * (1. - 2. * (f64::from(y) + f64::from(uv.y)) / n))
            .sinh()
            .atan()
            .to_degrees();
        basis.project(basis.surface_position(lat, lon), rect)
    };
    let mut quads = Vec::new();
    for row in 0..TILE_GRID_SEGMENTS {
        for col in 0..TILE_GRID_SEGMENTS {
            let uv = |col: u32, row: u32| {
                Pos2::new(
                    col as f32 / TILE_GRID_SEGMENTS as f32,
                    row as f32 / TILE_GRID_SEGMENTS as f32,
                )
            };
            let texture_coordinates = [
                uv(col, row),
                uv(col + 1, row),
                uv(col + 1, row + 1),
                uv(col, row + 1),
            ];
            if let [Some(a), Some(b), Some(c), Some(d)] = texture_coordinates.map(project) {
                quads.push(TileQuad {
                    positions: [a, b, c, d],
                    texture_coordinates,
                });
            }
        }
    }
    quads
}

pub fn project_outline(
    basis: &Basis,
    rect: Rect,
    points: &[[f64; 2]],
    closed: bool,
) -> Vec<[Pos2; 2]> {
    let mut lines = Vec::new();
    for (index, p) in points.iter().enumerate() {
        let end = points
            .get(index + 1)
            .or_else(|| closed.then(|| points.first()).flatten());
        if let Some(q) = end {
            let arc = basis
                .model
                .path((p[1], p[0]), (q[1], q[0]), OUTLINE_ARC_SEGMENTS);
            for pair in arc.windows(2) {
                if let (Some(a), Some(b)) =
                    (basis.project(pair[0], rect), basis.project(pair[1], rect))
                {
                    lines.push([a, b]);
                }
            }
        }
    }
    lines
}
