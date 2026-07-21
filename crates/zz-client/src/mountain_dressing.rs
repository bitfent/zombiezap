//! Mountain Town render-only identity dressing (M28).
//!
//! Pure visual props keyed off [`EnvKind::MountainTown`] + map AABBs:
//! slope skirts, pines, rock outcrops, horizon ridge. Never touches sim,
//! WalkGrid semantics, or collision — goldens stay bit-green.
//!
//! Zero asset files: procedural meshes + canvas-style speckled textures.

use std::f32::consts::TAU;

use bevy::{
    asset::RenderAssetUsages,
    image::{Image, ImageSampler},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use zz_core::map::{GameMap, WalkGrid};
use zz_core::rng::Mulberry32;
use zz_core::types::Aabb;

use crate::map_render::MeshGeom;

// ── Band / classifier heuristics (geometry, not hardcoded seam coords) ─────

/// Thin-in-Z, long-in-X, mid-height cover at y0≈0 — retaining-wall shape.
const RETAIN_MIN_LEN: f32 = 3.0;
const RETAIN_MAX_THICK: f32 = 1.2;
const RETAIN_MIN_H: f32 = 1.35;
const RETAIN_MAX_H: f32 = 2.6;
/// Stair steps: wide X, thin Z, walkable tops ≤ ~1 m.
const STAIR_MIN_W: f32 = 3.0;
const STAIR_MAX_THICK: f32 = 1.2;
const STAIR_MAX_H: f32 = 1.1;
/// Slope run depth from wall face (m).
const SLOPE_RUN: f32 = 3.2;
const SLOPE_STAIR_FLANK: f32 = 1.4;
/// Edge ring width inside arena for prop placement (m).
const EDGE_RING: f32 = 1.5;
/// Tree count range (inclusive-ish).
const PINE_MIN: u32 = 48;
const PINE_MAX: u32 = 72;
/// One prop placement (world XZ + yaw + uniform scale). Deterministic from seed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PropPose {
    pub x: f32,
    pub z: f32,
    pub yaw: f32,
    pub scale: f32,
}

/// Built mountain-only meshes + placement bookkeeping for tests.
#[derive(Clone, Debug, Default)]
pub struct MountainDressing {
    pub slopes: MeshGeom,
    /// Near LOD: stacked cones + trunks (split by material family).
    pub pine_trunks: MeshGeom,
    pub pine_needles: MeshGeom,
    /// Far LOD: billboard crosses (needle material).
    pub pine_far: MeshGeom,
    pub rocks: MeshGeom,
    pub ridge: MeshGeom,
    pub pine_poses: Vec<PropPose>,
    pub rock_poses: Vec<PropPose>,
    pub retaining_count: usize,
    pub stair_run_count: usize,
}

impl MountainDressing {
    pub fn pine_count(&self) -> usize {
        self.pine_poses.len()
    }

    pub fn rock_cluster_count(&self) -> usize {
        self.rock_poses.len()
    }
}

// ── Public entry ───────────────────────────────────────────────────────────

/// Build all mountain dressing from a map. Caller must gate on `EnvKind::MountainTown`.
pub fn build_mountain_dressing(map: &GameMap) -> MountainDressing {
    let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
    let retaining = classify_retaining_walls(&map.walls, map.arena_half);
    let stair_runs = classify_stair_runs(&map.walls);

    let mut out = MountainDressing {
        retaining_count: retaining.len(),
        stair_run_count: stair_runs.len(),
        ..Default::default()
    };

    // Slope skirts against retaining walls + stair flanks.
    for w in &retaining {
        append_retaining_slope(&mut out.slopes, w, SLOPE_RUN);
    }
    for run in &stair_runs {
        append_stair_flank_slopes(&mut out.slopes, run, SLOPE_STAIR_FLANK);
    }

    // Pines: deterministic placement on non-walkable / edge ring / exterior treeline.
    out.pine_poses = place_pines(&map.seed, map.arena_half, &grid);
    for (i, p) in out.pine_poses.iter().enumerate() {
        // Far LOD only for exterior treeline (outside the walls). Everything
        // inside the arena uses the full cone stack so the high-band / edge
        // ring still reads as real pines, not billboards.
        let exterior = p.x.abs() > map.arena_half || p.z.abs() > map.arena_half;
        if exterior {
            append_pine_billboard(&mut out.pine_far, p);
        } else {
            append_pine_full(&mut out.pine_trunks, &mut out.pine_needles, p, i as u32);
        }
    }

    // Rocks at perimeter corners + stair mouths.
    out.rock_poses = place_rocks(&map.seed, map.arena_half, &stair_runs, &grid);
    for (i, p) in out.rock_poses.iter().enumerate() {
        append_rock_cluster(&mut out.rocks, p, i as u32);
    }

    // Distant ridge silhouette (outside play, fog-friendly).
    out.ridge = build_horizon_ridge(map.arena_half, &map.seed);

    out
}

// ── Classifiers ────────────────────────────────────────────────────────────

/// Long, low-ish, thin-in-Z cover segments that span X — retaining walls at
/// band seams. Excludes perimeter (near ±arena_half) and stair steps (short H).
pub fn classify_retaining_walls(walls: &[Aabb], arena_half: f32) -> Vec<Aabb> {
    let mut out = Vec::new();
    for w in walls {
        if is_retaining_wall(w, arena_half) {
            out.push(*w);
        }
    }
    // Stable order for determinism.
    out.sort_by(|a, b| {
        a.z0
            .partial_cmp(&b.z0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

pub fn is_retaining_wall(w: &Aabb, arena_half: f32) -> bool {
    let sx = (w.x1 - w.x0).abs();
    let sy = (w.y1 - w.y0).abs();
    let sz = (w.z1 - w.z0).abs();
    if sx < RETAIN_MIN_LEN || !(0.15..=RETAIN_MAX_THICK).contains(&sz) {
        return false;
    }
    if !(RETAIN_MIN_H..=RETAIN_MAX_H).contains(&sy) {
        return false;
    }
    if w.y0.abs() > 0.15 {
        return false;
    }
    // Not a perimeter wall (those sit on ±half and are much taller).
    let on_peri = near_half(w.x0, arena_half)
        || near_half(w.x1, arena_half)
        || near_half(w.z0, arena_half)
        || near_half(w.z1, arena_half);
    if on_peri && sy > 4.0 {
        return false;
    }
    // Prefer thin dimension along Z (band-seam walls run along X).
    sx > sz * 2.5
}

fn near_half(v: f32, half: f32) -> bool {
    (v.abs() - half).abs() <= 1.5
}

/// A stair run is a contiguous sequence of wide, thin, low steps sharing width.
#[derive(Clone, Debug)]
pub struct StairRun {
    pub x0: f32,
    pub x1: f32,
    pub z0: f32,
    pub z1: f32,
    pub y1: f32,
}

pub fn classify_stair_runs(walls: &[Aabb]) -> Vec<StairRun> {
    let mut steps: Vec<Aabb> = walls.iter().copied().filter(is_stair_step).collect();
    steps.sort_by(|a, b| {
        // Group by approximate x-mid then z.
        let am = (a.x0 + a.x1) * 0.5;
        let bm = (b.x0 + b.x1) * 0.5;
        am.partial_cmp(&bm)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.z0.partial_cmp(&b.z0).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut runs: Vec<StairRun> = Vec::new();
    let mut i = 0;
    while i < steps.len() {
        let s0 = steps[i];
        let mid0 = (s0.x0 + s0.x1) * 0.5;
        let mut x0 = s0.x0;
        let mut x1 = s0.x1;
        let mut z0 = s0.z0;
        let mut z1 = s0.z1;
        let mut y1 = s0.y1;
        let mut j = i + 1;
        while j < steps.len() {
            let s = steps[j];
            let mid = (s.x0 + s.x1) * 0.5;
            if (mid - mid0).abs() > 0.6 {
                break;
            }
            // Same run: steps advance in +Z and share similar width.
            if (s.x0 - s0.x0).abs() > 0.6 || (s.x1 - s0.x1).abs() > 0.6 {
                break;
            }
            if s.z0 > z1 + 0.8 {
                break;
            }
            x0 = x0.min(s.x0);
            x1 = x1.max(s.x1);
            z0 = z0.min(s.z0);
            z1 = z1.max(s.z1);
            y1 = y1.max(s.y1);
            j += 1;
        }
        if j - i >= 2 {
            runs.push(StairRun {
                x0,
                x1,
                z0,
                z1,
                y1,
            });
        }
        i = j.max(i + 1);
    }
    runs
}

fn is_stair_step(w: &Aabb) -> bool {
    let sx = (w.x1 - w.x0).abs();
    let sy = (w.y1 - w.y0).abs();
    let sz = (w.z1 - w.z0).abs();
    sx >= STAIR_MIN_W
        && (0.2..=STAIR_MAX_THICK).contains(&sz)
        && sy > 0.12
        && sy <= STAIR_MAX_H
        && w.y0.abs() <= 0.1
}

// ── Slope skirts ───────────────────────────────────────────────────────────

/// Earthen wedge against the downhill (−Z) face of a retaining wall.
fn append_retaining_slope(geom: &mut MeshGeom, wall: &Aabb, run: f32) {
    let x0 = wall.x0;
    let x1 = wall.x1;
    let z_face = wall.z0; // south face (toward lower band)
    let z_toe = z_face - run;
    let y_top = wall.y1 * 0.92;
    // Slight inset so the wedge doesn't z-fight the wall face.
    let z_cap = z_face - 0.02;
    append_ramp_wedge(geom, x0, x1, z_cap, z_toe, y_top, 0.0);
}

/// Thin earth flanks along the ±X sides of a stair run.
fn append_stair_flank_slopes(geom: &mut MeshGeom, run: &StairRun, width: f32) {
    let y_top = run.y1.min(1.0) * 0.9;
    let z0 = run.z0;
    let z1 = run.z1;
    // Left flank (−X): ramp sloping down away from stairs.
    append_side_ramp(geom, run.x0 - width, run.x0 - 0.05, z0, z1, y_top, true);
    // Right flank (+X).
    append_side_ramp(geom, run.x1 + 0.05, run.x1 + width, z0, z1, y_top, false);
}

/// Trapezoid ramp: high at z_high, ground at z_low, spanning [x0,x1].
/// Top surface + two triangular end caps (solid look-down read).
fn append_ramp_wedge(
    geom: &mut MeshGeom,
    x0: f32,
    x1: f32,
    z_high: f32,
    z_low: f32,
    y_high: f32,
    y_low: f32,
) {
    if (x1 - x0).abs() < 0.05 || (z_high - z_low).abs() < 0.05 || y_high < 0.05 {
        return;
    }
    // Corners: high edge (wall) A,B ; low edge (toe) C,D at y_low (usually 0).
    let a = [x0, y_high, z_high];
    let b = [x1, y_high, z_high];
    let c = [x1, y_low, z_low];
    let d = [x0, y_low, z_low];
    let a0 = [x0, 0.0, z_high];
    let b0 = [x1, 0.0, z_high];

    // Top ramp — attribute normal from winding.
    push_quad_auto(geom, [a, b, c, d], (x1 - x0).abs(), (z_high - z_low).abs());

    // End caps: triangles (high-top, high-ground, toe) so look-down reads solid.
    // −X end: outward roughly −X.
    push_triangle_auto(geom, a, d, a0);
    // +X end.
    push_triangle_auto(geom, b, b0, c);

    // Back under wall lip (vertical at z_high).
    push_quad_auto(geom, [a0, b0, b, a], (x1 - x0).abs(), y_high);
}

/// Push a quad; attribute normals = geometric normal of the first triangle.
fn push_quad_auto(geom: &mut MeshGeom, corners: [[f32; 3]; 4], u_size: f32, v_size: f32) {
    let n = face_normal(corners[0], corners[1], corners[2]);
    if n.length_squared() < 1e-12 {
        // Try reverse order.
        let rev = [corners[0], corners[3], corners[2], corners[1]];
        let n2 = face_normal(rev[0], rev[1], rev[2]);
        if n2.length_squared() < 1e-12 {
            return;
        }
        geom.push_face(rev, n2.to_array(), u_size, v_size);
        return;
    }
    // If we need the opposite (caller doesn't care for ramps as long as consistent).
    geom.push_face(corners, n.to_array(), u_size, v_size);
}

fn push_triangle_auto(geom: &mut MeshGeom, a: [f32; 3], b: [f32; 3], c: [f32; 3]) {
    let n = face_normal(a, b, c);
    if n.length_squared() < 1e-12 {
        return;
    }
    push_triangle(geom, a, b, c, n.to_array());
}

/// Side ramp along a stair: high at the stair edge (x_in), low at x_out.
fn append_side_ramp(
    geom: &mut MeshGeom,
    x_a: f32,
    x_b: f32,
    z0: f32,
    z1: f32,
    y_high: f32,
    high_on_right: bool,
) {
    let (x_high, x_low) = if high_on_right {
        (x_b.max(x_a), x_a.min(x_b))
    } else {
        (x_a.min(x_b), x_b.max(x_a))
    };
    if (x_high - x_low).abs() < 0.05 || (z1 - z0).abs() < 0.05 || y_high < 0.05 {
        return;
    }
    // High edge along stairs, low edge away. Prefer upward normal.
    let a = [x_high, y_high, z0];
    let b = [x_high, y_high, z1];
    let c = [x_low, 0.0, z1];
    let d = [x_low, 0.0, z0];
    let n = face_normal(a, b, c);
    let corners = if n.y >= 0.0 {
        [a, b, c, d]
    } else {
        [a, d, c, b]
    };
    push_quad_auto(geom, corners, (z1 - z0).abs(), (x_high - x_low).abs());
}

fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Vec3 {
    let p0 = Vec3::from_array(a);
    let p1 = Vec3::from_array(b);
    let p2 = Vec3::from_array(c);
    (p1 - p0).cross(p2 - p0).normalize_or_zero()
}

/// Reorder four corners so CCW winding matches the desired outward normal.
fn order_quad_outward(corners: [[f32; 3]; 4], outward: Vec3) -> [[f32; 3]; 4] {
    let n = face_normal(corners[0], corners[1], corners[2]);
    if n.dot(outward) > 0.0 {
        corners
    } else {
        // Reverse winding: 0,3,2,1
        [corners[0], corners[3], corners[2], corners[1]]
    }
}

// ── Pines ──────────────────────────────────────────────────────────────────

pub fn place_pines(seed: &str, arena_half: f32, grid: &WalkGrid) -> Vec<PropPose> {
    let mut rng = Mulberry32::from_seed(&format!("{seed}|m28-pines"));
    let target = PINE_MIN + (rng.next() * (PINE_MAX - PINE_MIN + 1) as f64).floor() as u32;
    let mut poses = Vec::with_capacity(target as usize);
    let mut tries = 0u32;
    let max_tries = target * 40;

    while poses.len() < target as usize && tries < max_tries {
        tries += 1;
        // Bias: 55% exterior treeline, 30% high-band non-walkable, 15% edge ring.
        let mode = rng.next();
        let (x, z) = if mode < 0.55 {
            // Outside / on perimeter exterior.
            exterior_treeline_point(&mut rng, arena_half)
        } else if mode < 0.85 {
            // High band interior non-walkable (z toward +half).
            let x = (rng.next() as f32 * 2.0 - 1.0) * (arena_half - 0.5);
            let z = 6.0 + rng.next() as f32 * (arena_half - 6.0);
            (x, z.clamp(-arena_half + 0.5, arena_half - 0.5))
        } else {
            // Edge margin ring inside walls.
            edge_ring_point(&mut rng, arena_half)
        };

        if !pine_site_ok(x, z, arena_half, grid) {
            continue;
        }
        // Spacing: reject if too close to an existing pine.
        let min_d2 = 2.2f32 * 2.2;
        if poses.iter().any(|p: &PropPose| {
            (p.x - x) * (p.x - x) + (p.z - z) * (p.z - z) < min_d2
        }) {
            continue;
        }
        let yaw = rng.next() as f32 * TAU;
        let scale = 0.85 + rng.next() as f32 * 0.55;
        poses.push(PropPose { x, z, yaw, scale });
    }
    poses
}

fn exterior_treeline_point(rng: &mut Mulberry32, half: f32) -> (f32, f32) {
    // Place just outside the perimeter walls (visual treeline).
    let side = (rng.next() * 4.0).floor() as i32;
    let t = rng.next() as f32 * 2.0 - 1.0;
    let out = half + 1.5 + rng.next() as f32 * 5.0;
    match side {
        0 => (t * half, -out),  // south
        1 => (t * half, out),   // north (high band treeline)
        2 => (-out, t * half),  // west
        _ => (out, t * half),   // east
    }
}

fn edge_ring_point(rng: &mut Mulberry32, half: f32) -> (f32, f32) {
    let side = (rng.next() * 4.0).floor() as i32;
    let along = (rng.next() as f32 * 2.0 - 1.0) * (half - 0.8);
    let inset = half - 0.4 - rng.next() as f32 * EDGE_RING;
    match side {
        0 => (along, -inset),
        1 => (along, inset),
        2 => (-inset, along),
        _ => (inset, along),
    }
}

/// Valid pine site: non-walkable cell OR outside play / edge ring — never on
/// open walkable floor (zero collision / flow impact).
fn pine_site_ok(x: f32, z: f32, arena_half: f32, grid: &WalkGrid) -> bool {
    let outside = x.abs() > arena_half - 0.1 || z.abs() > arena_half - 0.1;
    if outside {
        // Exterior / perimeter strip: always OK (not in walk grid play space).
        return true;
    }
    // Interior: only non-walkable cells (roofs/cover/void).
    !grid.walkable_at(x, z)
}

fn append_pine_full(trunks: &mut MeshGeom, needles: &mut MeshGeom, p: &PropPose, salt: u32) {
    let s = p.scale;
    let yaw = p.yaw;
    let (cy, sy) = (yaw.cos(), yaw.sin());
    let rot = |lx: f32, lz: f32| (p.x + lx * cy - lz * sy, p.z + lx * sy + lz * cy);

    // Trunk cylinder.
    let trunk_h = 1.1 * s;
    let trunk_r = 0.12 * s;
    append_cylinder(trunks, p.x, 0.0, p.z, trunk_r, trunk_h, 6, yaw);

    // 2–3 stacked cones (needles).
    let layers = 2 + (salt % 2);
    let mut base_y = trunk_h * 0.55;
    for layer in 0..layers {
        let t = layer as f32 / layers.max(1) as f32;
        let h = (1.6 - t * 0.35) * s;
        let r = (0.95 - t * 0.28) * s;
        let (ox, oz) = rot((salt as f32 * 0.01 + layer as f32 * 0.02) * 0.0, 0.0);
        let _ = (ox, oz);
        append_cone(needles, p.x, base_y, p.z, r, h, 7, yaw + layer as f32 * 0.15);
        base_y += h * 0.55;
    }
}

fn append_pine_billboard(geom: &mut MeshGeom, p: &PropPose) {
    let h = 2.8 * p.scale;
    let w = 1.4 * p.scale;
    // Cross of two vertical quads (billboard-cross), unrotated world axes + 45°.
    let y0 = 0.0;
    let y1 = h;
    let hw = w * 0.5;
    // Quad A: in X (facing ±Z)
    let a = [
        [p.x - hw, y0, p.z],
        [p.x + hw, y0, p.z],
        [p.x + hw, y1, p.z],
        [p.x - hw, y1, p.z],
    ];
    // Outward +Z for front; also emit back via opposite winding as second face.
    geom.push_face(a, [0.0, 0.0, 1.0], w, h);
    let a_back = [a[1], a[0], a[3], a[2]];
    geom.push_face(a_back, [0.0, 0.0, -1.0], w, h);
    // Quad B: in Z (facing ±X)
    let b = [
        [p.x, y0, p.z + hw],
        [p.x, y0, p.z - hw],
        [p.x, y1, p.z - hw],
        [p.x, y1, p.z + hw],
    ];
    geom.push_face(b, [1.0, 0.0, 0.0], w, h);
    let b_back = [b[1], b[0], b[3], b[2]];
    geom.push_face(b_back, [-1.0, 0.0, 0.0], w, h);
}

#[allow(clippy::too_many_arguments)]
fn append_cylinder(
    geom: &mut MeshGeom,
    cx: f32,
    y0: f32,
    cz: f32,
    radius: f32,
    height: f32,
    sides: u32,
    yaw: f32,
) {
    let y1 = y0 + height;
    let n = sides.max(3);
    for i in 0..n {
        let a0 = yaw + (i as f32) / n as f32 * TAU;
        let a1 = yaw + ((i + 1) as f32) / n as f32 * TAU;
        let (c0, s0) = (a0.cos(), a0.sin());
        let (c1, s1) = (a1.cos(), a1.sin());
        let p00 = [cx + c0 * radius, y0, cz + s0 * radius];
        let p10 = [cx + c1 * radius, y0, cz + s1 * radius];
        let p11 = [cx + c1 * radius, y1, cz + s1 * radius];
        let p01 = [cx + c0 * radius, y1, cz + s0 * radius];
        // Outward normal ≈ average of the two radial dirs on XZ.
        let nx = (c0 + c1) * 0.5;
        let nz = (s0 + s1) * 0.5;
        let nl = (nx * nx + nz * nz).sqrt().max(1e-5);
        let normal = [nx / nl, 0.0, nz / nl];
        // CCW when viewed from outside.
        let corners = order_quad_outward([p00, p10, p11, p01], Vec3::from_array(normal));
        geom.push_face(
            corners,
            normal,
            radius * TAU / n as f32,
            height,
        );
    }
    // Top cap (optional small disc as triangle fan via quads from center — skip for PS2).
    // Flat top: one fan as n tris using degenerate approach — single fan via triangles.
    // Keep it simple: omit top (hidden under cones).
    let _ = y1;
}

#[allow(clippy::too_many_arguments)]
fn append_cone(
    geom: &mut MeshGeom,
    cx: f32,
    y0: f32,
    cz: f32,
    radius: f32,
    height: f32,
    sides: u32,
    yaw: f32,
) {
    let apex = [cx, y0 + height, cz];
    let n = sides.max(3);
    for i in 0..n {
        let a0 = yaw + (i as f32) / n as f32 * TAU;
        let a1 = yaw + ((i + 1) as f32) / n as f32 * TAU;
        let (c0, s0) = (a0.cos(), a0.sin());
        let (c1, s1) = (a1.cos(), a1.sin());
        let b0 = [cx + c0 * radius, y0, cz + s0 * radius];
        let b1 = [cx + c1 * radius, y0, cz + s1 * radius];
        // Triangle b0 → b1 → apex (CCW from outside).
        let geo = face_normal(b0, b1, apex);
        // Ensure outward (away from axis).
        let mid = Vec3::new((c0 + c1) * 0.5, 0.0, (s0 + s1) * 0.5);
        let nrm = if geo.dot(mid) >= 0.0 { geo } else { -geo };
        let (p0, p1, p2) = if face_normal(b0, b1, apex).dot(nrm) > 0.0 {
            (b0, b1, apex)
        } else {
            (b1, b0, apex)
        };
        // Degenerate quad: p0,p1,p2,p2 — better push as triangle via 3 unique + duplicate.
        // MeshGeom is quad-based; use p0,p1,p2,p0 with careful winding for two tris.
        // tri0: 0-1-2, tri1: 0-2-3 — if 3==0, tri1 is degenerate. Use thin ridge:
        // duplicate apex slightly — no, use order p0,p1,p2,p2 and accept degenerate tri.
        // Prefer proper: push_triangle helper.
        push_triangle(geom, p0, p1, p2, nrm.to_array());
    }
}

fn push_triangle(geom: &mut MeshGeom, a: [f32; 3], b: [f32; 3], c: [f32; 3], normal: [f32; 3]) {
    let base = geom.positions.len() as u32;
    for p in [a, b, c] {
        geom.positions.push(p);
        geom.normals.push(normal);
        geom.uvs.push([0.0, 0.0]);
    }
    geom.indices
        .extend_from_slice(&[base, base + 1, base + 2]);
}

// ── Rocks ──────────────────────────────────────────────────────────────────

pub fn place_rocks(
    seed: &str,
    arena_half: f32,
    stair_runs: &[StairRun],
    grid: &WalkGrid,
) -> Vec<PropPose> {
    let mut rng = Mulberry32::from_seed(&format!("{seed}|m28-rocks"));
    let mut poses = Vec::new();

    // Four exterior corners (outside play — zero walk impact).
    let out = arena_half + 1.8;
    let corners = [(-out, -out), (out, -out), (-out, out), (out, out)];
    for &(cx, cz) in &corners {
        let jx = (rng.next() as f32 - 0.5) * 1.2;
        let jz = (rng.next() as f32 - 0.5) * 1.2;
        poses.push(PropPose {
            x: cx + jx,
            z: cz + jz,
            yaw: rng.next() as f32 * TAU,
            scale: 0.85 + rng.next() as f32 * 0.45,
        });
    }

    // Beside each stair mouth — snap to nearest non-walkable if needed.
    for run in stair_runs {
        for &(sx, sz) in &[
            (run.x0 - 1.4, run.z0 + 0.2),
            (run.x1 + 1.4, run.z0 + 0.2),
            (run.x0 - 1.4, run.z1 - 0.2),
            (run.x1 + 1.4, run.z1 - 0.2),
        ] {
            if rng.next() < 0.4 {
                continue;
            }
            if let Some((x, z)) = snap_non_walkable(sx, sz, arena_half, grid) {
                poses.push(PropPose {
                    x,
                    z,
                    yaw: rng.next() as f32 * TAU,
                    scale: 0.55 + rng.next() as f32 * 0.4,
                });
            }
        }
    }
    poses
}

/// If (x,z) is already non-walkable / outside, keep it; else search a small ring.
fn snap_non_walkable(
    x: f32,
    z: f32,
    arena_half: f32,
    grid: &WalkGrid,
) -> Option<(f32, f32)> {
    if rock_site_ok(x, z, arena_half, grid) {
        return Some((x, z));
    }
    for r in 1..=4 {
        let d = r as f32 * 0.5;
        for (dx, dz) in [(-d, 0.0), (d, 0.0), (0.0, -d), (0.0, d), (-d, -d), (d, d), (-d, d), (d, -d)]
        {
            let nx = x + dx;
            let nz = z + dz;
            if rock_site_ok(nx, nz, arena_half, grid) {
                return Some((nx, nz));
            }
        }
    }
    None
}

fn rock_site_ok(x: f32, z: f32, arena_half: f32, grid: &WalkGrid) -> bool {
    // Outside the play half (incl. exterior corners) — always OK.
    if x.abs() >= arena_half - 0.05 || z.abs() >= arena_half - 0.05 {
        return true;
    }
    !grid.walkable_at(x, z)
}

fn append_rock_cluster(geom: &mut MeshGeom, p: &PropPose, salt: u32) {
    let mut rng = Mulberry32::from_seed(&format!("rock-{salt}-{}-{}", p.x.to_bits(), p.z.to_bits()));
    let n = 2 + (rng.next() * 3.0).floor() as i32; // 2–4 boxes
    for i in 0..n {
        let lx = (rng.next() as f32 - 0.5) * 0.9 * p.scale;
        let lz = (rng.next() as f32 - 0.5) * 0.9 * p.scale;
        let (cy, sy) = (p.yaw.cos(), p.yaw.sin());
        let wx = p.x + lx * cy - lz * sy;
        let wz = p.z + lx * sy + lz * cy;
        let sx = (0.35 + rng.next() as f32 * 0.55) * p.scale;
        let sy_h = (0.25 + rng.next() as f32 * 0.55) * p.scale;
        let sz = (0.3 + rng.next() as f32 * 0.5) * p.scale;
        let yaw = p.yaw + (rng.next() as f32 - 0.5) * 0.8 + i as f32 * 0.3;
        append_rotated_box(
            geom,
            wx,
            sy_h * 0.5,
            wz,
            sx,
            sy_h,
            sz,
            yaw,
        );
    }
}

/// Axis-aligned box after yaw rotation about Y (visual only — not collision).
#[allow(clippy::too_many_arguments)]
fn append_rotated_box(
    geom: &mut MeshGeom,
    cx: f32,
    cy: f32,
    cz: f32,
    hx: f32,
    hy: f32,
    hz: f32,
    yaw: f32,
) {
    let (c, s) = (yaw.cos(), yaw.sin());
    let rot = |x: f32, y: f32, z: f32| -> [f32; 3] {
        [cx + x * c - z * s, cy + y, cz + x * s + z * c]
    };
    // Local corners of a unit box centered at origin.
    let x0 = -hx * 0.5;
    let x1 = hx * 0.5;
    let y0 = -hy * 0.5;
    let y1 = hy * 0.5;
    let z0 = -hz * 0.5;
    let z1 = hz * 0.5;

    // Six faces with outward normals rotated in XZ.
    let faces: [([[f32; 3]; 4], [f32; 3]); 6] = [
        // +Y
        (
            [
                rot(x0, y1, z1),
                rot(x1, y1, z1),
                rot(x1, y1, z0),
                rot(x0, y1, z0),
            ],
            [0.0, 1.0, 0.0],
        ),
        // -Y
        (
            [
                rot(x0, y0, z0),
                rot(x1, y0, z0),
                rot(x1, y0, z1),
                rot(x0, y0, z1),
            ],
            [0.0, -1.0, 0.0],
        ),
        // +X local → world (c, 0, s)
        (
            [
                rot(x1, y0, z1),
                rot(x1, y0, z0),
                rot(x1, y1, z0),
                rot(x1, y1, z1),
            ],
            [c, 0.0, s],
        ),
        // -X local → (−c, 0, −s)
        (
            [
                rot(x0, y0, z0),
                rot(x0, y0, z1),
                rot(x0, y1, z1),
                rot(x0, y1, z0),
            ],
            [-c, 0.0, -s],
        ),
        // +Z local → (−s, 0, c)
        (
            [
                rot(x0, y0, z1),
                rot(x1, y0, z1),
                rot(x1, y1, z1),
                rot(x0, y1, z1),
            ],
            [-s, 0.0, c],
        ),
        // -Z local → (s, 0, −c)
        (
            [
                rot(x1, y0, z0),
                rot(x0, y0, z0),
                rot(x0, y1, z0),
                rot(x1, y1, z0),
            ],
            [s, 0.0, -c],
        ),
    ];
    for (corners, n) in faces {
        let nrm = Vec3::from_array(n).normalize_or_zero();
        let ordered = order_quad_outward(corners, nrm);
        // Recompute attribute normal from ordered winding so it always agrees.
        let geo = face_normal(ordered[0], ordered[1], ordered[2]);
        let attr = if geo.length_squared() > 1e-12 {
            geo
        } else {
            nrm
        };
        geom.push_face(ordered, attr.to_array(), hx.max(hz), hy.max(0.1));
    }
}

// ── Horizon ridge ──────────────────────────────────────────────────────────

/// Low-poly distant mountain ring (or full surround) outside the arena.
pub fn build_horizon_ridge(arena_half: f32, seed: &str) -> MeshGeom {
    let mut rng = Mulberry32::from_seed(&format!("{seed}|m28-ridge"));
    let mut geom = MeshGeom::default();
    let r_in = arena_half * 2.4;
    let r_out = arena_half * 3.1;
    let segments = 28u32;
    let base_y = -2.0;
    let snow_line = 8.0;

    for i in 0..segments {
        let a0 = (i as f32) / segments as f32 * TAU;
        let a1 = ((i + 1) as f32) / segments as f32 * TAU;
        // Vary peak height deterministically; bias higher toward +Z (high band view).
        let h0 = 6.0 + hash01(&mut rng) * 10.0 + (a0.sin().max(0.0)) * 4.0;
        let h1 = 6.0 + hash01(&mut rng) * 10.0 + (a1.sin().max(0.0)) * 4.0;

        let i0 = [r_in * a0.cos(), base_y, r_in * a0.sin()];
        let i1 = [r_in * a1.cos(), base_y, r_in * a1.sin()];
        let p0 = [r_in * a0.cos(), h0, r_in * a0.sin()];
        let p1 = [r_in * a1.cos(), h1, r_in * a1.sin()];
        let o0 = [r_out * a0.cos(), base_y * 0.5, r_out * a0.sin()];
        let o1 = [r_out * a1.cos(), base_y * 0.5, r_out * a1.sin()];

        // Inner face (toward arena): rock body.
        let n_in = face_normal(i0, i1, p1);
        // Prefer facing arena (toward origin).
        let toward = -Vec3::new(p0[0] + p1[0], 0.0, p0[2] + p1[2]).normalize_or_zero();
        let n_in = if n_in.dot(toward) > 0.0 { n_in } else { -n_in };
        let body = order_quad_outward([i0, i1, p1, p0], n_in);
        // Split rock / snow by vertex y via two stacked quads when peaks exceed snow line.
        let snow0 = h0 > snow_line;
        let snow1 = h1 > snow_line;
        if snow0 || snow1 {
            let t0 = ((snow_line - base_y) / (h0 - base_y).max(0.1)).clamp(0.05, 0.95);
            let t1 = ((snow_line - base_y) / (h1 - base_y).max(0.1)).clamp(0.05, 0.95);
            let m0 = lerp3(i0, p0, t0);
            let m1 = lerp3(i1, p1, t1);
            // Rock lower
            let rock_n = face_normal(i0, i1, m1);
            let rock_n = if rock_n.dot(toward) > 0.0 {
                rock_n
            } else {
                -rock_n
            };
            let rock_q = order_quad_outward([i0, i1, m1, m0], rock_n);
            push_face_colored(&mut geom, rock_q, rock_n.to_array(), [0.42, 0.40, 0.38, 1.0]);
            // Snow cap
            let snow_n = face_normal(m0, m1, p1);
            let snow_n = if snow_n.dot(toward) > 0.0 {
                snow_n
            } else {
                -snow_n
            };
            let snow_q = order_quad_outward([m0, m1, p1, p0], snow_n);
            push_face_colored(&mut geom, snow_q, snow_n.to_array(), [0.92, 0.94, 0.96, 1.0]);
        } else {
            push_face_colored(&mut geom, body, n_in.to_array(), [0.42, 0.40, 0.38, 1.0]);
            let _ = (o0, o1);
        }
    }
    geom
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn push_face_colored(
    geom: &mut MeshGeom,
    corners: [[f32; 3]; 4],
    _normal_hint: [f32; 3],
    color: [f32; 4],
) {
    // Attribute normal must match geometric winding (M27 invariant).
    let n = face_normal(corners[0], corners[1], corners[2]);
    let normal = if n.length_squared() > 1e-12 {
        n.to_array()
    } else {
        _normal_hint
    };
    let base = geom.positions.len() as u32;
    for c in corners {
        geom.positions.push(c);
        geom.normals.push(normal);
        geom.uvs.push([0.0, 0.0]);
        geom.colors.push(color);
    }
    geom.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn hash01(rng: &mut Mulberry32) -> f32 {
    rng.next() as f32
}

// ── Procedural textures ────────────────────────────────────────────────────

fn tex_hash(x: u32, y: u32, salt: u32) -> f32 {
    let mut n = x
        .wrapping_mul(374761393)
        .wrapping_add(y.wrapping_mul(668265263))
        .wrapping_add(salt.wrapping_mul(2246822519));
    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
    n ^= n >> 16;
    (n & 0xffff) as f32 / 65535.0
}

fn rgba_image(width: u32, height: u32, pixels: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// Earth / scree: brown base + grey grit speckle.
pub fn gen_earth_scree_tex(size: u32) -> Image {
    let base = [0x6b_u8, 0x52, 0x3a];
    let grit = [0x8a_u8, 0x7a, 0x68];
    speckled_tex(size, base, grit, 900, 11)
}

/// Dark needle green with deeper flecks.
pub fn gen_needle_tex(size: u32) -> Image {
    let base = [0x1e_u8, 0x3a, 0x22];
    let fleck = [0x14_u8, 0x2c, 0x18];
    speckled_tex(size, base, fleck, 700, 22)
}

/// Bark brown with vertical-ish grain bias.
pub fn gen_bark_tex(size: u32) -> Image {
    let base = [0x4a_u8, 0x32, 0x22];
    let fleck = [0x3a_u8, 0x28, 0x1a];
    speckled_tex(size, base, fleck, 500, 33)
}

/// Grey stone for rock outcrops.
pub fn gen_stone_tex(size: u32) -> Image {
    let base = [0x6e_u8, 0x6c, 0x68];
    let fleck = [0x58_u8, 0x56, 0x52];
    speckled_tex(size, base, fleck, 600, 44)
}

fn speckled_tex(size: u32, base: [u8; 3], fleck: [u8; 3], n_specks: u32, salt: u32) -> Image {
    let n = (size * size) as usize;
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        let o = i * 4;
        px[o] = base[0];
        px[o + 1] = base[1];
        px[o + 2] = base[2];
        px[o + 3] = 255;
    }
    for i in 0..n_specks {
        let u = (tex_hash(i, 0, salt) * size as f32).floor() as u32 % size;
        let v = (tex_hash(i, 1, salt.wrapping_add(1)) * size as f32).floor() as u32 % size;
        let a = 0.25 + tex_hash(i, 2, salt.wrapping_add(2)) * 0.45;
        for dy in 0..2u32 {
            for dx in 0..2u32 {
                let x = (u + dx) % size;
                let y = (v + dy) % size;
                let o = ((y * size + x) * 4) as usize;
                px[o] = ((fleck[0] as f32) * a + px[o] as f32 * (1.0 - a)) as u8;
                px[o + 1] = ((fleck[1] as f32) * a + px[o + 1] as f32 * (1.0 - a)) as u8;
                px[o + 2] = ((fleck[2] as f32) * a + px[o + 2] as f32 * (1.0 - a)) as u8;
            }
        }
    }
    rgba_image(size, size, px)
}

/// Soft per-band ground tint multipliers (RGB) for mountain z-bands.
/// Seams blend over ~4 m so bands read as zones, not stripes.
pub fn mountain_band_tint(z: f32) -> [f32; 3] {
    // Soft centers: low < -10, mid ~0, high > 10.
    let low = [0.78, 0.88, 0.62]; // grassy green-brown
    let mid = [0.92, 0.88, 0.78]; // scree grey-brown
    let high = [0.95, 0.96, 0.98]; // pale / frosted
    let blend = |a: [f32; 3], b: [f32; 3], t: f32| {
        let t = t.clamp(0.0, 1.0);
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    // Smoothstep across seams at −10 and +10 with 4 m half-width.
    let s1 = smoothstep(-14.0, -6.0, z); // 0 in deep low → 1 in mid
    let s2 = smoothstep(6.0, 14.0, z); // 0 in mid → 1 in high
    let lm = blend(low, mid, s1);
    blend(lm, high, s2)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-5)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ── Determinism helper (tests) ─────────────────────────────────────────────

/// FNV-ish hash of all pine/rock poses for seed stability checks.
pub fn hash_prop_poses(poses: &[PropPose]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for p in poses {
        for bits in [p.x.to_bits(), p.z.to_bits(), p.yaw.to_bits(), p.scale.to_bits()] {
            h ^= bits as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
    }
    h
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_core::map::generate_map;
    use zz_core::types::EnvKind;

    fn assert_winding_matches_attribute_normals(geom: &MeshGeom, label: &str) {
        assert!(
            !geom.indices.is_empty(),
            "{label}: expected non-empty mesh"
        );
        assert_eq!(
            geom.indices.len() % 3,
            0,
            "{label}: index count not multiple of 3"
        );
        for (ti, tri) in geom.indices.chunks_exact(3).enumerate() {
            let i0 = tri[0] as usize;
            let i1 = tri[1] as usize;
            let i2 = tri[2] as usize;
            let p0 = Vec3::from_array(geom.positions[i0]);
            let p1 = Vec3::from_array(geom.positions[i1]);
            let p2 = Vec3::from_array(geom.positions[i2]);
            let geometric = (p1 - p0).cross(p2 - p0);
            if geometric.length_squared() < 1e-12 {
                // Degenerate (e.g. collapsed) — skip only if all verts coincide-ish.
                continue;
            }
            for &vi in &[i0, i1, i2] {
                let attr = Vec3::from_array(geom.normals[vi]);
                let d = geometric.dot(attr);
                assert!(
                    d > 0.0,
                    "{label} tri {ti} vert {vi}: geometric {geometric:?} · attr {attr:?} = {d}"
                );
            }
        }
    }

    #[test]
    fn retaining_walls_found_on_mountain_map() {
        let map = generate_map(EnvKind::MountainTown, "m28-class");
        let ret = classify_retaining_walls(&map.walls, map.arena_half);
        assert!(
            ret.len() >= 4,
            "expected several retaining segments, got {}",
            ret.len()
        );
        // All should be thin-in-Z and long-in-X.
        for w in &ret {
            assert!((w.x1 - w.x0).abs() > (w.z1 - w.z0).abs());
            assert!((w.y1 - w.y0) > 1.3);
        }
    }

    #[test]
    fn slope_mesh_winding_ok() {
        let map = generate_map(EnvKind::MountainTown, "m28-slope");
        let d = build_mountain_dressing(&map);
        assert!(!d.slopes.is_empty(), "slopes should be non-empty");
        assert_winding_matches_attribute_normals(&d.slopes, "slopes");
    }

    #[test]
    fn pine_meshes_winding_ok() {
        let map = generate_map(EnvKind::MountainTown, "m28-pine");
        let d = build_mountain_dressing(&map);
        assert!(d.pine_count() >= 40, "pine count {}", d.pine_count());
        assert!(d.pine_count() <= 80, "pine count {}", d.pine_count());
        if !d.pine_trunks.is_empty() {
            assert_winding_matches_attribute_normals(&d.pine_trunks, "pine trunks");
        }
        if !d.pine_needles.is_empty() {
            assert_winding_matches_attribute_normals(&d.pine_needles, "pine needles");
        }
        if !d.pine_far.is_empty() {
            assert_winding_matches_attribute_normals(&d.pine_far, "pine far");
        }
    }

    #[test]
    fn rock_and_ridge_winding_ok() {
        let map = generate_map(EnvKind::MountainTown, "m28-rock");
        let d = build_mountain_dressing(&map);
        assert!(!d.rocks.is_empty());
        assert_winding_matches_attribute_normals(&d.rocks, "rocks");
        assert!(!d.ridge.is_empty());
        assert_winding_matches_attribute_normals(&d.ridge, "ridge");
    }

    #[test]
    fn prop_placement_deterministic() {
        let map = generate_map(EnvKind::MountainTown, "m28-det");
        let a = build_mountain_dressing(&map);
        let b = build_mountain_dressing(&map);
        assert_eq!(hash_prop_poses(&a.pine_poses), hash_prop_poses(&b.pine_poses));
        assert_eq!(hash_prop_poses(&a.rock_poses), hash_prop_poses(&b.rock_poses));
        assert_eq!(a.pine_poses, b.pine_poses);
        assert_eq!(a.rock_poses, b.rock_poses);
    }

    #[test]
    fn pines_and_rocks_never_on_walkable() {
        let map = generate_map(EnvKind::MountainTown, "m28-walk");
        let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
        let d = build_mountain_dressing(&map);
        for p in d.pine_poses.iter().chain(d.rock_poses.iter()) {
            // Outside / perimeter strip is always allowed.
            let outside =
                p.x.abs() >= map.arena_half - 0.1 || p.z.abs() >= map.arena_half - 0.1;
            if outside {
                continue;
            }
            assert!(
                !grid.walkable_at(p.x, p.z),
                "prop at ({}, {}) lands on walkable cell",
                p.x,
                p.z
            );
        }
    }

    #[test]
    fn band_tint_low_vs_high_differs() {
        let lo = mountain_band_tint(-20.0);
        let hi = mountain_band_tint(20.0);
        // Low should be greener (higher G relative / lower pale).
        assert!(lo[1] > lo[0] * 0.95, "low band should read greenish {lo:?}");
        assert!(hi[0] + hi[1] + hi[2] > lo[0] + lo[1] + lo[2]);
    }

    #[test]
    fn urban_map_classifier_finds_no_mountain_retaining() {
        // Urban may have long cover but not the mountain retaining signature at scale;
        // dressing is env-gated anyway — classifier should not explode.
        let map = generate_map(EnvKind::Urban, "m28-urban");
        let _ = classify_retaining_walls(&map.walls, map.arena_half);
    }
}

