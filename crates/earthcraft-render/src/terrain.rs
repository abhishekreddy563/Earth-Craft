//! Terrain displacement for the native EarthCraft raycaster.
//! Elevation is read from the *same* asynchronous disk/memory tile cache as imagery.
//! There is no HTML/WebView/MapLibre dependency.
use super::{Local, LocalTile, Lookup, Provider, RenderCfg, TileKey, make_key, rays};
use earthcraft_core::{EARTH_RADIUS_M, earth_model::SurfaceGeometry};
use glam::DVec3;
use std::f64::consts::PI;

const DEM_MAX_Z: i32 = 15;
const DEM_TILE_EDGE: f64 = 512.0;

/// Look up a Terrarium height in metres, seeking available parent tiles while
/// tracking misses for the existing background downloader.
fn height_at(
    loc: &mut Local<impl Fn(TileKey) -> Lookup>,
    z: i32,
    u: f64,
    v: f64,
    dist: f32,
) -> Option<(f64, i32)> {
    if !(0.0..=1.0).contains(&v) {
        return None;
    }
    let u = u.rem_euclid(1.0);
    for level in (0..=z.clamp(0, DEM_MAX_Z)).rev() {
        let n = (1u64 << level) as f64;
        let tx = (u * n).floor().min(n - 1.0);
        let ty = (v * n).floor().min(n - 1.0);
        let key = make_key(Provider::TerrainDem, level as u8, tx as u32, ty as u32, 0);
        let tile = loc
            .map
            .entry(key)
            .or_insert_with(|| match (loc.lookup)(key) {
                Lookup::Ready(tile) => LocalTile::Ready(tile),
                Lookup::Pending => LocalTile::Pending,
                Lookup::Absent => LocalTile::Absent(dist),
            });
        match tile {
            LocalTile::Ready(tile) => {
                return Some((tile.sample_terrarium_m(u * n - tx, v * n - ty), level));
            }
            LocalTile::Pending => {}
            LocalTile::Absent(nearest) => *nearest = nearest.min(dist),
        }
    }
    None
}

/// Inverse-spherical Mercator for a normal's geographic latitude/longitude.
fn uv_from_normal(n: DVec3) -> (f64, f64) {
    let lat = n.z.asin().to_degrees().clamp(-85.05112878, 85.05112878);
    let lon = n.y.atan2(n.x);
    let v = 0.5 - lat.to_radians().tan().asinh() / (2.0 * PI);
    ((lon / (2.0 * PI) + 0.5).rem_euclid(1.0), v)
}

/// Correct a ray's intersection from reference ellipsoid to DEM altitude.
/// Uses two iterations along its viewing direction, enough for terrain shading
/// and parallax without remeshing the globe or changing its satellite imagery.
/// Very oblique rays are left at the ellipsoid to avoid singular corrections.
pub(super) fn displace(
    ray: &rays::RaySample,
    surface_uv: (f64, f64),
    image_zoom: i32,
    dist: f32,
    geometry: SurfaceGeometry,
    cfg: &RenderCfg,
    loc: &mut Local<impl Fn(TileKey) -> Lookup>,
) -> (rays::RaySample, f32) {
    let Some(terrain) = cfg.terrain else {
        return (*ray, 1.0);
    };
    if ray.incidence < 0.18 {
        return (*ray, 1.0);
    }
    let dem_zoom = (image_zoom - 1).clamp(0, DEM_MAX_Z);
    let exaggeration = terrain.exaggeration.clamp(0.5, 3.0) as f64;
    let (u, v) = surface_uv;
    let Some((mut elevation_m, loaded_zoom)) = height_at(loc, dem_zoom, u, v, dist) else {
        return (*ray, 1.0);
    };
    let mut adjusted = *ray;
    let mut height_sample = (elevation_m, loaded_zoom);
    for _ in 0..2 {
        // The ray direction is normalized in globe-radius units, so the
        // metres-to-radius conversion is essential here.
        let delta = elevation_m * exaggeration / EARTH_RADIUS_M / ray.incidence;
        let t = ray.distance - delta;
        // Do not cross or enter the camera when zoomed below mountain tops.
        if t <= 0.0 || !t.is_finite() {
            break;
        }
        adjusted.distance = t;
        adjusted.point = ray.point + ray.direction * (t - ray.distance);
        adjusted.normal = geometry.normal(adjusted.point);
        adjusted.incidence = (-ray.direction.dot(adjusted.normal)).max(0.0);
        let (uu, vv) = uv_from_normal(adjusted.normal);
        if let Some(next) = height_at(loc, dem_zoom, uu, vv, dist) {
            elevation_m = next.0;
            height_sample = next;
        } else {
            break;
        }
    }

    // Estimate tangents from the elevation grid to make cliffs and ridges
    // visible even without the application's optional day/night sunlight.
    let (u, v) = uv_from_normal(adjusted.normal);
    let actual_z = height_sample.1;
    let tile_step = 1.0 / (DEM_TILE_EDGE * (1u64 << actual_z) as f64);
    let east_h = height_at(loc, actual_z, u + tile_step, v, dist)
        .map(|h| h.0)
        .unwrap_or(elevation_m);
    let south_h = height_at(loc, actual_z, u, v + tile_step, dist)
        .map(|h| h.0)
        .unwrap_or(elevation_m);
    let coslat = adjusted.normal.z.acos().sin().max(0.015);
    let horizontal_m = 2.0 * PI * EARTH_RADIUS_M * coslat * tile_step;
    let slope_e = ((east_h - elevation_m) / horizontal_m * exaggeration).clamp(-5.0, 5.0);
    let slope_n = ((elevation_m - south_h) / horizontal_m * exaggeration).clamp(-5.0, 5.0);
    let east = DVec3::new(-adjusted.normal.y, adjusted.normal.x, 0.0).normalize_or_zero();
    let north = adjusted.normal.cross(east).normalize_or_zero();
    let mountain_normal = (adjusted.normal - east * slope_e - north * slope_n).normalize();
    let light = (adjusted.normal * 0.7 + north * 0.55 - east * 0.45).normalize();
    let flat_illumination = adjusted.normal.dot(light);
    let relief_illumination = mountain_normal.dot(light).max(0.0);
    let shade = (0.9 + 0.7 * (relief_illumination - flat_illumination)).clamp(0.58, 1.25);
    (adjusted, shade as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mercator_normal_wraps_dateline_and_places_equator_at_midpoint() {
        let (u, v) = uv_from_normal(DVec3::X);
        assert!((u - 0.5).abs() < 1e-12);
        assert!((v - 0.5).abs() < 1e-12);
        let (_, northern_v) = uv_from_normal(DVec3::new(0.7071067812, 0.0, 0.7071067812));
        assert!(northern_v < 0.5);
    }
}
