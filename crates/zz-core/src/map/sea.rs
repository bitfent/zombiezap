//! Sea Town generator.
//!
//! Shoreline strip on +z where the ground is *cosmetic* water — a knee-high
//! dock edge keeps play on land, piers extend as elevated bridge-style decks
//! into the water band, and big two-room warehouses sit inland.

use super::coop::{
    GameMap, footprint_of, inflate_xz, make_building, make_l_wall, near_points, overlaps_xz,
    perimeter_billboards, perimeter_gates, perimeter_walls, snap_f32, spawn_cluster,
};
use super::grid::WalkGrid;
use super::{ACCENTS, ARENA_HALF, DECK_BOTTOM, DECK_TOP, EDGE_MARGIN, GAP, SPAWN_CLEAR};
use crate::rng::Mulberry32;
use crate::types::{Aabb, EnvKind};

/// z ≥ SHORE is the cosmetic water band (no floor colliders — client paints it).
const SHORE: f32 = 12.0;
/// Dock edge height: just above WalkGrid::MAX_FLOOR (1.0) so it actually blocks.
const DOCK_H: f32 = 1.15;
const WAREHOUSE_H: f32 = 3.6;

pub fn generate(seed: &str) -> GameMap {
    let half = ARENA_HALF as f32;
    let mut rng = Mulberry32::from_seed(&format!("{seed}|sea"));
    let lim = half - EDGE_MARGIN as f32;

    // Spawn inland on the dry side (south-west).
    let spawn_d = half - 4.0;
    let ax = -spawn_d;
    let az = -spawn_d;
    let yaw = -core::f32::consts::PI * 0.75;
    let anchors = [(ax, az)];

    let mut walls = perimeter_walls(half);
    let mut cover: Vec<Aabb> = Vec::new();

    // ── knee-high dock edge along the shoreline ───────────────────────────
    // Gaps at pier roots so players can step onto piers; edge still fences
    // most of the water band off for ground-level pathing.
    let pier_xs = [-16.0f32, -2.0, 14.0];
    let gap_w = 4.0f32;
    {
        let t = 0.5f32;
        let z0 = snap_f32(SHORE - t / 2.0);
        let z1 = snap_f32(SHORE + t / 2.0);
        let mut edges: Vec<f32> = vec![-half + 0.5];
        for &px in &pier_xs {
            edges.push(px - gap_w / 2.0);
            edges.push(px + gap_w / 2.0);
        }
        edges.push(half - 0.5);
        let mut i = 0;
        while i + 1 < edges.len() {
            let a = edges[i];
            let b = edges[i + 1];
            if i % 2 == 0 && b - a > 0.6 {
                cover.push(Aabb {
                    x0: snap_f32(a),
                    x1: snap_f32(b),
                    y0: 0.0,
                    y1: DOCK_H,
                    z0,
                    z1,
                });
            }
            i += 1;
        }
    }

    // ── piers (bridge-deck style elevated boards into the water) ───────────
    // Thin elevated decks: y0 ≥ CLEARANCE so the 2-D ground grid still marks
    // cells walkable underneath; the dock edge is what keeps AI/play on land
    // except at pier gaps. Players use step-up onto pier end posts.
    for &px in &pier_xs {
        let pier_w = 3.0 + (rng.next() as f32) * 0.6;
        let pier_len = 8.0 + (rng.next() as f32) * 4.0;
        let z_start = SHORE - 1.0;
        let z_end = (z_start + pier_len).min(half - 1.5);
        // landside step-up plinth (walkable top ≤ 1.0)
        cover.push(Aabb {
            x0: snap_f32(px - pier_w / 2.0),
            x1: snap_f32(px + pier_w / 2.0),
            y0: 0.0,
            y1: 1.0,
            z0: snap_f32(z_start - 1.2),
            z1: snap_f32(z_start + 0.4),
        });
        // elevated deck over water
        cover.push(Aabb {
            x0: snap_f32(px - pier_w / 2.0),
            x1: snap_f32(px + pier_w / 2.0),
            y0: DECK_BOTTOM as f32,
            y1: DECK_TOP as f32,
            z0: snap_f32(z_start),
            z1: snap_f32(z_end),
        });
        // end post / bollard
        cover.push(Aabb {
            x0: snap_f32(px - 0.35),
            x1: snap_f32(px + 0.35),
            y0: 0.0,
            y1: DECK_TOP as f32,
            z0: snap_f32(z_end - 0.5),
            z1: snap_f32(z_end + 0.2),
        });
    }

    let group_fits = |segs: &[Aabb], cover: &[Aabb]| -> bool {
        let fp = footprint_of(segs);
        if fp.x0 < -lim || fp.x1 > lim || fp.z0 < -lim || fp.z1 > lim {
            return false;
        }
        // keep warehouses on the dry side of the dock
        if fp.z1 > SHORE - 1.0 {
            return false;
        }
        if near_points(&fp, &anchors, SPAWN_CLEAR as f32) {
            return false;
        }
        !cover
            .iter()
            .any(|c| overlaps_xz(&inflate_xz(&fp, GAP as f32), c))
    };

    // ── big two-room warehouses ───────────────────────────────────────────
    let wh_n = 3 + if rng.next() < 0.5 { 1 } else { 0 };
    let mut placed = 0;
    let mut tries = 0;
    while placed < wh_n && tries < 70 {
        tries += 1;
        let w = 8.0 + (rng.next() as f32) * 4.0;
        let d = 7.0 + (rng.next() as f32) * 3.0;
        let cx = ((rng.next() as f32) * 2.0 - 1.0) * (lim - w / 2.0 - 0.5);
        // dry land only: z from -lim up to SHORE - d/2 - 2
        let z_max = SHORE - d / 2.0 - 2.5;
        let z_min = -lim + d / 2.0;
        if z_max <= z_min {
            break;
        }
        let cz = z_min + (rng.next() as f32) * (z_max - z_min);
        let segs = make_building(&mut rng, cx, cz, w, d, WAREHOUSE_H, true);
        if group_fits(&segs, &cover) {
            cover.extend(segs);
            placed += 1;
        }
    }

    // Smaller sheds inland.
    let shed_n = 2 + if rng.next() < 0.4 { 1 } else { 0 };
    let mut s_placed = 0;
    let mut s_try = 0;
    while s_placed < shed_n && s_try < 40 {
        s_try += 1;
        let w = 4.5 + (rng.next() as f32) * 2.0;
        let d = 4.0 + (rng.next() as f32) * 1.5;
        let cx = ((rng.next() as f32) * 2.0 - 1.0) * (lim - w / 2.0);
        let z_max = SHORE - d / 2.0 - 2.0;
        let z_min = -lim + d / 2.0;
        if z_max <= z_min {
            break;
        }
        let cz = z_min + (rng.next() as f32) * (z_max - z_min);
        let segs = make_building(&mut rng, cx, cz, w, d, 2.8, false);
        if group_fits(&segs, &cover) {
            cover.extend(segs);
            s_placed += 1;
        }
    }

    // Street cover on the dry side.
    let cover_n = 8 + (rng.next() * 5.0).floor() as i32;
    let mut c_placed = 0;
    let mut c_try = 0;
    while c_placed < cover_n && c_try < 180 {
        c_try += 1;
        let w = 0.7 + (rng.next() as f32) * 1.0;
        let d = 0.7 + (rng.next() as f32) * 1.0;
        let h = 1.1 + (rng.next() as f32) * 0.9;
        let cx = snap_f32(((rng.next() as f32) * 2.0 - 1.0) * (lim - w));
        let cz = snap_f32(-lim + 1.0 + (rng.next() as f32) * (SHORE - 3.0 + lim));
        if cz > SHORE - 2.0 {
            continue;
        }
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
            c_placed += 1;
        }
    }

    let l_n = 2 + if rng.next() < 0.5 { 1 } else { 0 };
    for _ in 0..l_n {
        for _try in 0..24 {
            let cx = ((rng.next() as f32) * 2.0 - 1.0) * (lim - 5.0);
            let cz = -lim + 2.0 + (rng.next() as f32) * (SHORE + lim - 8.0);
            if cz > SHORE - 3.0 {
                continue;
            }
            let segs = make_l_wall(&mut rng, cx, cz, 1.5);
            if group_fits(&segs, &cover) {
                cover.extend(segs);
                break;
            }
        }
    }

    walls.extend(cover);

    let grid = WalkGrid::rasterize(&walls, half);
    let (sx, sz) = if grid.walkable_at(ax, az) {
        (ax, az)
    } else {
        (ax + 2.0, az + 2.0)
    };
    let spawns = spawn_cluster(sx, sz, yaw, &grid);
    let gates = perimeter_gates(&grid, &spawns, half);
    let billboards = perimeter_billboards(seed, half);

    let avoid: Vec<(f32, f32)> = spawns.iter().map(|s| (s.x, s.z)).collect();
    let mut rng_pick = Mulberry32::from_seed(&format!("{seed}|sea-pick"));
    // Dry-side pickups only (z well below the dock edge).
    let pickups = scatter_pickups_dry(&walls, half, &mut rng_pick, &avoid, 4, SHORE - 3.0);
    let accent = ACCENTS[(rng.next() * ACCENTS.len() as f64).floor() as usize];

    GameMap {
        seed: seed.to_string(),
        env: EnvKind::SeaTown,
        arena_half: half,
        walls,
        spawns,
        gates,
        billboards,
        pickups,
        accent,
    }
}

/// Like `scatter_pickups` but rejects samples with z ≥ `z_max` (stay on dry land).
fn scatter_pickups_dry(
    walls: &[Aabb],
    half: f32,
    rng: &mut Mulberry32,
    avoid: &[(f32, f32)],
    count: usize,
    z_max: f32,
) -> Vec<(f32, f32)> {
    let grid = WalkGrid::rasterize(walls, half);
    let lim = half - 1.5;
    let mut out = Vec::with_capacity(count);
    let mut tries = 0;
    while out.len() < count && tries < 240 {
        tries += 1;
        let x = ((rng.next() as f32) * 2.0 - 1.0) * lim;
        let z = -lim + (rng.next() as f32) * ((z_max + lim).max(1.0));
        if z > z_max {
            continue;
        }
        if !grid.walkable_at(x, z) {
            continue;
        }
        if avoid
            .iter()
            .chain(out.iter())
            .any(|(ax, az)| (ax - x) * (ax - x) + (az - z) * (az - z) < 6.0 * 6.0)
        {
            continue;
        }
        out.push((x, z));
    }
    // Deterministic scan for remaining slots on dry walkable ground.
    if out.len() < count {
        let n = grid.cells_per_side;
        'scan: for iz in 0..n {
            for ix in 0..n {
                if !grid.is_walkable(ix, iz) {
                    continue;
                }
                let x = -half + (ix as f32 + 0.5);
                let z = -half + (iz as f32 + 0.5);
                if z > z_max {
                    continue;
                }
                if out
                    .iter()
                    .any(|(ax, az)| (ax - x) * (ax - x) + (az - z) * (az - z) < 4.0)
                {
                    continue;
                }
                out.push((x, z));
                if out.len() >= count {
                    break 'scan;
                }
            }
        }
    }
    while out.len() < count {
        out.push((0.0, -10.0));
    }
    out
}
