//! Ray math for hitscan: ray vs AABB (wall occlusion) and ray vs sphere
//! (hitboxes). Port of legacy/packages/shared/src/raycast.ts. All
//! transcendentals go through `libm` so native and wasm32 agree bit-for-bit.

use crate::types::Aabb;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }
}

/// Forward vector from yaw/pitch (three.js 'YXZ' convention: -Z is forward).
/// Kept identical to ShotAnte so ported aim math behaves the same.
pub fn dir_from_angles(yaw: f32, pitch: f32) -> Vec3 {
    let cp = libm::cosf(pitch);
    Vec3 {
        x: -libm::sinf(yaw) * cp,
        y: libm::sinf(pitch),
        z: -libm::cosf(yaw) * cp,
    }
}

/// Slab test. Distance t >= 0 to entry, or None.
pub fn ray_box(o: Vec3, d: Vec3, b: &Aabb) -> Option<f32> {
    let mut tmin = f32::NEG_INFINITY;
    let mut tmax = f32::INFINITY;
    for (ov, dv, lo, hi) in [
        (o.x, d.x, b.x0, b.x1),
        (o.y, d.y, b.y0, b.y1),
        (o.z, d.z, b.z0, b.z1),
    ] {
        if dv.abs() < 1e-9 {
            if ov < lo || ov > hi {
                return None;
            }
            continue;
        }
        let mut t1 = (lo - ov) / dv;
        let mut t2 = (hi - ov) / dv;
        if t1 > t2 {
            core::mem::swap(&mut t1, &mut t2);
        }
        tmin = tmin.max(t1);
        tmax = tmax.min(t2);
        if tmin > tmax {
            return None;
        }
    }
    if tmax < 0.0 {
        return None;
    }
    Some(tmin.max(0.0))
}

/// Distance t >= 0 to the sphere, or None.
pub fn ray_sphere(o: Vec3, d: Vec3, c: Vec3, r: f32) -> Option<f32> {
    let ox = o.x - c.x;
    let oy = o.y - c.y;
    let oz = o.z - c.z;
    let b = ox * d.x + oy * d.y + oz * d.z;
    let cc = ox * ox + oy * oy + oz * oz - r * r;
    let disc = b * b - cc;
    if disc < 0.0 {
        return None;
    }
    let t = -b - libm::sqrtf(disc);
    if t < 0.0 {
        // inside or behind
        return if cc <= 0.0 { Some(0.0) } else { None };
    }
    Some(t)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallHit {
    pub t: f32,
    pub index: usize,
}

/// Nearest wall hit along a ray — hitscan occlusion needs the distance,
/// destructibles need to know WHICH wall stopped the ray.
pub fn nearest_wall_hit(o: Vec3, d: Vec3, walls: &[Aabb]) -> Option<WallHit> {
    let mut best: Option<WallHit> = None;
    for (index, w) in walls.iter().enumerate() {
        if let Some(t) = ray_box(o, d, w)
            && best.is_none_or(|b| t < b.t)
        {
            best = Some(WallHit { t, index });
        }
    }
    best
}

pub fn nearest_wall_t(o: Vec3, d: Vec3, walls: &[Aabb]) -> Option<f32> {
    nearest_wall_hit(o, d, walls).map(|h| h.t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: Aabb = Aabb {
        x0: -1.0,
        x1: 1.0,
        y0: 0.0,
        y1: 2.0,
        z0: 4.0,
        z1: 6.0,
    };

    #[test]
    fn ray_hits_box_straight_on() {
        let t = ray_box(Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0), &BOX);
        assert_eq!(t, Some(4.0));
    }

    #[test]
    fn ray_misses_box_beside_it() {
        let t = ray_box(Vec3::new(3.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0), &BOX);
        assert_eq!(t, None);
    }

    #[test]
    fn ray_behind_box_misses() {
        let t = ray_box(Vec3::new(0.0, 1.0, 8.0), Vec3::new(0.0, 0.0, 1.0), &BOX);
        assert_eq!(t, None);
    }

    #[test]
    fn ray_from_inside_box_reports_zero() {
        let t = ray_box(Vec3::new(0.0, 1.0, 5.0), Vec3::new(0.0, 0.0, 1.0), &BOX);
        assert_eq!(t, Some(0.0));
    }

    #[test]
    fn parallel_ray_inside_slab_hits() {
        let t = ray_box(Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0), &BOX);
        assert!(t.is_some());
    }

    #[test]
    fn sphere_straight_on_and_miss() {
        let c = Vec3::new(0.0, 0.0, 10.0);
        let t = ray_sphere(Vec3::default(), Vec3::new(0.0, 0.0, 1.0), c, 2.0).unwrap();
        assert!((t - 8.0).abs() < 1e-5);
        assert!(ray_sphere(Vec3::new(5.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0), c, 2.0).is_none());
    }

    #[test]
    fn inside_sphere_reports_zero() {
        let t = ray_sphere(
            Vec3::default(),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::default(),
            1.0,
        );
        assert_eq!(t, Some(0.0));
    }

    #[test]
    fn nearest_wall_picks_closest_and_reports_index() {
        let far = Aabb {
            z0: 8.0,
            z1: 9.0,
            ..BOX
        };
        let hit = nearest_wall_hit(
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            &[far, BOX],
        )
        .unwrap();
        assert_eq!(hit.index, 1);
        assert_eq!(hit.t, 4.0);
    }

    #[test]
    fn dir_from_angles_forward_is_minus_z() {
        let d = dir_from_angles(0.0, 0.0);
        assert!((d.z + 1.0).abs() < 1e-6 && d.x.abs() < 1e-6 && d.y.abs() < 1e-6);
    }
}
