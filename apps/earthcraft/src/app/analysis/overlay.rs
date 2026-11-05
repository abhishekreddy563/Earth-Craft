//! Project the selected region and a bounded XYZ preview to the displayed globe.
use super::Analysis;
use earthcraft_core::camera::Basis;
use earthcraft_data::analysis::AnalysisClient;
use eframe::egui::{self, Color32, Rect, Stroke};
use serde_json::Value;

#[derive(Clone, Copy)]
pub(super) struct Tile {
    z: u8,
    x: u32,
    y: u32,
}
const MAX_TILE_ZOOM: u8 = 16;
const MAX_PREVIEW_TILES: u32 = 9;

pub(super) fn positions(geometry: &Value) -> Vec<[f64; 2]> {
    let coords = &geometry["coordinates"];
    let line = if geometry["type"] == "Polygon" {
        &coords[0]
    } else {
        coords
    };
    line.as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some([p[0].as_f64()?, p[1].as_f64()?]))
        .collect()
}

fn tile_at(p: [f64; 2], z: u8) -> (u32, u32) {
    let n = f64::from(1u32 << z);
    let x = (p[0] + 180.0) / 360.0 * n;
    let y = (1.0
        - p[1]
            .clamp(-85.05112878, 85.05112878)
            .to_radians()
            .tan()
            .asinh()
            / std::f64::consts::PI)
        * 0.5
        * n;
    (x.clamp(0.0, n - 1.0) as u32, y.clamp(0.0, n - 1.0) as u32)
}
fn coverage(points: &[[f64; 2]]) -> Vec<Tile> {
    if points.is_empty() {
        return Vec::new();
    }
    for z in (0..=MAX_TILE_ZOOM).rev() {
        let (mut west, mut north, mut east, mut south) = (u32::MAX, u32::MAX, 0, 0);
        for &p in points {
            let (x, y) = tile_at(p, z);
            west = west.min(x);
            east = east.max(x);
            north = north.min(y);
            south = south.max(y);
        }
        if u64::from(east - west + 1) * u64::from(south - north + 1) <= u64::from(MAX_PREVIEW_TILES)
        {
            return (north..=south)
                .flat_map(|y| (west..=east).map(move |x| Tile { z, x, y }))
                .collect();
        }
    }
    Vec::new()
}
pub(super) async fn load(
    client: &AnalysisClient,
    template: &str,
    points: &[[f64; 2]],
) -> Result<Vec<(Tile, image::RgbaImage)>, String> {
    let mut result = Vec::new();
    for tile in coverage(points) {
        let path = template
            .replace("{z}", &tile.z.to_string())
            .replace("{x}", &tile.x.to_string())
            .replace("{y}", &tile.y.to_string());
        result.push((tile, client.tile(&path).await?));
    }
    Ok(result)
}
fn outline(
    painter: &egui::Painter,
    basis: &Basis,
    rect: Rect,
    points: &[[f64; 2]],
    color: Color32,
    closed: bool,
) {
    for &point in points {
        if let Some(pos) = basis.project(basis.surface_position(point[1], point[0]), rect) {
            painter.circle_filled(pos, 3.0, color);
        }
    }
    for segment in earthcraft_render::analysis::project_outline(basis, rect, points, closed) {
        painter.line_segment(segment, Stroke::new(2., color));
    }
}
impl Analysis {
    pub(in crate::app) fn paint(&self, painter: &egui::Painter, basis: &Basis, rect: Rect) {
        if self.visible {
            for (tile, texture) in &self.tiles {
                let mut mesh = egui::Mesh::with_texture(texture.id());
                for quad in earthcraft_render::analysis::project_xyz_tile(
                    basis, rect, tile.z, tile.x, tile.y,
                ) {
                    let i = mesh.vertices.len() as u32;
                    for (pos, uv) in quad.positions.into_iter().zip(quad.texture_coordinates) {
                        mesh.vertices.push(egui::epaint::Vertex {
                            pos,
                            uv,
                            color: Color32::WHITE,
                        });
                    }
                    mesh.indices
                        .extend_from_slice(&[i, i + 1, i + 2, i, i + 2, i + 3]);
                }
                painter.add(egui::Shape::mesh(mesh));
            }
            if let Some(result) = &self.result {
                let shape = &result["geometry"];
                outline(
                    painter,
                    basis,
                    rect,
                    &positions(shape),
                    Color32::LIGHT_GREEN,
                    false,
                );
            }
        }
        if self.open {
            outline(
                painter,
                basis,
                rect,
                &self.points,
                Color32::GOLD,
                self.polygon && self.points.len() > 2,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_stays_bounded_and_uses_xyz_order() {
        assert_eq!(tile_at([0., 0.], 2), (2, 2));
        let tiles = coverage(&[[-65., -10.], [-64., -9.]]);
        assert!(!tiles.is_empty() && tiles.len() <= MAX_PREVIEW_TILES as usize);
        assert!(tiles[0].z > 4);
    }
}
