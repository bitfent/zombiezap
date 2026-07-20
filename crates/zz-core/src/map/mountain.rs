//! Mountain Town generator.
//!
//! Layout: three Z-bands (low / mid / high) joined by wide stair runs through
//! retaining-wall openings, with small cabins on each band. Gates are placed
//! on both the low and high perimeter edges via the shared perimeter gate
//! finder.
//!
//! **Walkability note:** `WalkGrid` is a 2-D ground raster with `MAX_FLOOR =
//! 1.0`. Solid terrace slabs at y = 1.5 / 3.0 would either block entire bands
//! (BFS-broken gates) or require grid semantics that change urban
//! rasterization. Play therefore stays on y = 0; terrace *character* comes
//! from banded retaining walls, walkable stair risers (tops ≤ 1 m), and
//! cabins that grow taller on the high band. Decorative thin decks sit at
//! y ≥ 1.85 (above body clearance) so they read as raised ground without
//! blocking the flow field.

use super::coop::{
    GameMap, footprint_of, inflate_xz, make_building, make_l_wall, make_stair_run, near_points,
    overlaps_xz, perimeter_billboards, perimeter_gates, perimeter_walls, scatter_pickups,
    snap_f32, spawn_cluster,
};
use super::grid::WalkGrid;
use super::{ACCENTS, ARENA_HALF, EDGE_MARGIN, GAP, SPAWN_CLEAR};
use crate::rng::Mulberry32;
use crate::types::{Aabb, EnvKind};

/// Band seams (z): low < mid_z0 ≤ mid < high_z0 ≤ high.
const MID_Z0: f32 = -10.0;
const HIGH_Z0: f32 = 10.0;

pub fn generate(seed: &str) -> GameMap {
    let half = ARENA_HALF as f32;
    let mut rng = Mulberry32::from_seed(&format!("{seed}|mountain"));
    let lim = half - EDGE_MARGIN as f32;

    // Spawn cluster on the low (south-west) band, clear of cover.
    let spawn_d = half - 4.0;
    let ax = -spawn_d;
    let az = -spawn_d;
    let yaw = -core::f32::consts::PI * 0.75;
    let anchors = [(ax, az)];

    let mut walls = perimeter_walls(half);
    let mut cover: Vec<Aabb> = Vec::new();

    // ── retaining walls between bands (with wide stair openings) ──────────
    push_retaining_wall(&mut cover, half, MID_Z0, &mut rng);
    push_retaining_wall(&mut cover, half, HIGH_Z0, &mut rng);

    // Wide stair runs through the openings (walkable tops ≤ 1 m).
    // Mid seam stairs.
    for cx in [-14.0f32, 0.0, 14.0] {
        cover.extend(make_stair_run(cx, MID_Z0 - 0.5, 5.0, 1.0));
    }
    // High seam stairs (same openings as the retaining wall).
    for cx in [-14.0f32, 0.0, 14.0] {
        cover.extend(make_stair_run(cx, HIGH_Z0 - 0.5, 5.5, 1.0));
    }

    // Decorative elevated thin decks (above CLEARANCE so they don't block).
    // Mid band: y ≈ 1.85–2.0 reads as a raised terrace top.
    cover.push(thin_deck(-8.0, (MID_Z0 + HIGH_Z0) * 0.5, 10.0, 6.0, 1.85, 2.0));
    // High band: y ≈ 2.85–3.0 for the uppermost terrace read.
    cover.push(thin_deck(6.0, (HIGH_Z0 + half) * 0.5, 12.0, 8.0, 2.85, 3.0));

    let group_fits = |segs: &[Aabb], cover: &[Aabb]| -> bool {
        let fp = footprint_of(segs);
        if fp.x0 < -lim || fp.x1 > lim || fp.z0 < -lim || fp.z1 > lim {
            return false;
        }
        if near_points(&fp, &anchors, SPAWN_CLEAR as f32) {
            return false;
        }
        !cover
            .iter()
            .any(|c| overlaps_xz(&inflate_xz(&fp, GAP as f32), c))
    };

    // ── cabins on each band (small buildings) ─────────────────────────────
    // Low band: 2 cabins, short.
    place_cabins(
        &mut rng,
        &mut cover,
        &group_fits,
        2,
        -half + 6.0,
        MID_Z0 - 2.0,
        2.8,
    );
    // Mid band: 2–3 cabins.
    let mid_n = 2 + if rng.next() < 0.5 { 1 } else { 0 };
    place_cabins(
        &mut rng,
        &mut cover,
        &group_fits,
        mid_n,
        MID_Z0 + 2.0,
        HIGH_Z0 - 2.0,
        3.0,
    );
    // High band: 2 cabins, slightly taller.
    place_cabins(
        &mut rng,
        &mut cover,
        &group_fits,
        2,
        HIGH_Z0 + 2.0,
        half - 6.0,
        3.4,
    );

    // Street cover (L-walls + crates) so bands aren't empty corridors.
    let l_n = 2 + if rng.next() < 0.5 { 1 } else { 0 };
    for _ in 0..l_n {
        for _try in 0..24 {
            let cx = ((rng.next() as f32) * 2.0 - 1.0) * (lim - 5.0);
            let cz = ((rng.next() as f32) * 2.0 - 1.0) * (lim - 5.0);
            let segs = make_l_wall(&mut rng, cx, cz, 1.5);
            if group_fits(&segs, &cover) {
                cover.extend(segs);
                break;
            }
        }
    }
    let crate_n = 6 + (rng.next() * 4.0).floor() as i32;
    for _ in 0..crate_n {
        for _try in 0..20 {
            let w = 0.7 + (rng.next() as f32) * 0.8;
            let d = 0.7 + (rng.next() as f32) * 0.8;
            let h = 1.0 + (rng.next() as f32) * 0.9;
            let cx = snap_f32(((rng.next() as f32) * 2.0 - 1.0) * (lim - w));
            let cz = snap_f32(((rng.next() as f32) * 2.0 - 1.0) * (lim - d));
            let piece = Aabb {
                x0: cx - w,
                x1: cx + w,
                y0: 0.0,
                y1: h,
                z0: cz - d,
                z1: cz + d,
            };
            if group_fits(&[piece], &cover) {
                cover.push(piece);
                break;
            }
        }
    }

    walls.extend(cover);

    let grid = WalkGrid::rasterize(&walls, half);
    // If anchor isn't walkable (shouldn't happen — clear corner), nudge inward.
    let (sx, sz) = if grid.walkable_at(ax, az) {
        (ax, az)
    } else {
        find_walkable_near(&grid, ax, az).unwrap_or((ax + 2.0, az + 2.0))
    };
    let spawns = spawn_cluster(sx, sz, yaw, &grid);
    let gates = perimeter_gates(&grid, &spawns, half);
    let billboards = perimeter_billboards(seed, half);

    let avoid: Vec<(f32, f32)> = spawns.iter().map(|s| (s.x, s.z)).collect();
    let mut rng_pick = Mulberry32::from_seed(&format!("{seed}|mountain-pick"));
    let pickups = scatter_pickups(&walls, half, &mut rng_pick, &avoid, 4);
    let accent = ACCENTS[(rng.next() * ACCENTS.len() as f64).floor() as usize];

    GameMap {
        seed: seed.to_string(),
        env: EnvKind::MountainTown,
        arena_half: half,
        walls,
        spawns,
        gates,
        billboards,
        pickups,
        accent,
    }
}

fn thin_deck(cx: f32, cz: f32, w: f32, d: f32, y0: f32, y1: f32) -> Aabb {
    Aabb {
        x0: snap_f32(cx - w / 2.0),
        x1: snap_f32(cx + w / 2.0),
        y0,
        y1,
        z0: snap_f32(cz - d / 2.0),
        z1: snap_f32(cz + d / 2.0),
    }
}

/// Retaining wall across X at fixed z, with 3 openings for stairs.
fn push_retaining_wall(cover: &mut Vec<Aabb>, half: f32, z: f32, rng: &mut Mulberry32) {
    let t = 0.5f32;
    let h = 1.7 + (rng.next() as f32) * 0.4; // > MAX_FLOOR → blocks ground path
    let z0 = snap_f32(z - t / 2.0);
    let z1 = snap_f32(z + t / 2.0);
    // Three openings centered near the stair placements.
    let openings: [(f32, f32); 3] = [(-14.0, 5.5), (0.0, 5.5), (14.0, 5.5)];
    let mut edges: Vec<f32> = vec![-half + 1.0];
    for &(cx, w) in &openings {
        edges.push(cx - w / 2.0);
        edges.push(cx + w / 2.0);
    }
    edges.push(half - 1.0);
    // Pair edges into solid segments (skip opening spans).
    let mut i = 0;
    while i + 1 < edges.len() {
        let a = edges[i];
        let b = edges[i + 1];
        // even i → solid, odd i → opening
        if i % 2 == 0 && b - a > 0.8 {
            cover.push(Aabb {
                x0: snap_f32(a),
                x1: snap_f32(b),
                y0: 0.0,
                y1: h,
                z0,
                z1,
            });
        }
        i += 1;
    }
}

fn place_cabins(
    rng: &mut Mulberry32,
    cover: &mut Vec<Aabb>,
    group_fits: &dyn Fn(&[Aabb], &[Aabb]) -> bool,
    n: i32,
    z_lo: f32,
    z_hi: f32,
    h: f32,
) {
    let mut placed = 0;
    let mut tries = 0;
    while placed < n && tries < 50 {
        tries += 1;
        let w = 4.0 + (rng.next() as f32) * 2.5;
        let d = 4.0 + (rng.next() as f32) * 2.0;
        let cx = ((rng.next() as f32) * 2.0 - 1.0) * (ARENA_HALF as f32 - EDGE_MARGIN as f32 - w / 2.0);
        let span = (z_hi - z_lo).max(1.0);
        let cz = z_lo + (rng.next() as f32) * span;
        let segs = make_building(rng, cx, cz, w, d, h, false);
        if group_fits(&segs, cover) {
            cover.extend(segs);
            placed += 1;
        }
    }
}

fn find_walkable_near(grid: &WalkGrid, ax: f32, az: f32) -> Option<(f32, f32)> {
    for r in 1..=8 {
        for dz in -r..=r {
            for dx in -r..=r {
                let x = ax + dx as f32;
                let z = az + dz as f32;
                if grid.walkable_at(x, z) {
                    return Some((x, z));
                }
            }
        }
    }
    None
}
