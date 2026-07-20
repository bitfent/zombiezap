//! Desert Town generator.
//!
//! Low flat-roof adobe (h ≈ 2.6), walled compounds built from L-shaped walls,
//! a well centerpiece, open sightlines, and more street-level cover than urban.

use super::coop::{
    GameMap, footprint_of, inflate_xz, make_building, make_l_wall, near_points, overlaps_xz,
    perimeter_billboards, perimeter_gates, perimeter_walls, scatter_pickups, snap_f32,
    spawn_cluster,
};
use super::grid::WalkGrid;
use super::{ACCENTS, ARENA_HALF, EDGE_MARGIN, GAP, SPAWN_CLEAR};
use crate::rng::Mulberry32;
use crate::types::{Aabb, EnvKind};

const ADOBE_H: f32 = 2.6;

pub fn generate(seed: &str) -> GameMap {
    let half = ARENA_HALF as f32;
    let mut rng = Mulberry32::from_seed(&format!("{seed}|desert"));
    let lim = half - EDGE_MARGIN as f32;

    let spawn_d = half - 4.0;
    let ax = -spawn_d;
    let az = -spawn_d;
    let yaw = -core::f32::consts::PI * 0.75;
    let anchors = [(ax, az)];

    let mut walls = perimeter_walls(half);
    let mut cover: Vec<Aabb> = Vec::new();

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

    // ── well centerpiece ──────────────────────────────────────────────────
    // Ring of low adobe + central shaft so it reads as a well, not a block.
    {
        let r = 1.8f32;
        let t = 0.45f32;
        let h = 1.35f32;
        // four cardinal segments leaving diagonal gaps for pathing around
        let segs = [
            Aabb {
                x0: -r,
                x1: r,
                y0: 0.0,
                y1: h,
                z0: -r,
                z1: -r + t,
            },
            Aabb {
                x0: -r,
                x1: r,
                y0: 0.0,
                y1: h,
                z0: r - t,
                z1: r,
            },
            Aabb {
                x0: -r,
                x1: -r + t,
                y0: 0.0,
                y1: h,
                z0: -r + t,
                z1: r - t,
            },
            Aabb {
                x0: r - t,
                x1: r,
                y0: 0.0,
                y1: h,
                z0: -r + t,
                z1: r - t,
            },
            // central curb / shaft lip
            Aabb {
                x0: -0.55,
                x1: 0.55,
                y0: 0.0,
                y1: 0.85,
                z0: -0.55,
                z1: 0.55,
            },
        ];
        if group_fits(&segs, &cover) {
            cover.extend(segs);
        }
    }

    // ── adobe buildings (flat roof, low) ──────────────────────────────────
    let adobe_n = 4 + (rng.next() * 3.0).floor() as i32;
    let mut placed = 0;
    let mut tries = 0;
    while placed < adobe_n && tries < 80 {
        tries += 1;
        let w = 5.0 + (rng.next() as f32) * 3.0;
        let d = 5.0 + (rng.next() as f32) * 3.0;
        let cx = ((rng.next() as f32) * 2.0 - 1.0) * (lim - w / 2.0 - 0.5);
        // Bias away from exact center (well) and spawn corner.
        let cz = ((rng.next() as f32) * 2.0 - 1.0) * (lim - d / 2.0 - 0.5);
        if cx.abs() < 4.0 && cz.abs() < 4.0 {
            continue;
        }
        let segs = make_building(&mut rng, cx, cz, w, d, ADOBE_H, w.max(d) >= 7.5);
        if group_fits(&segs, &cover) {
            cover.extend(segs);
            placed += 1;
        }
    }

    // ── walled compounds from L-shaped walls ──────────────────────────────
    // Place 2–3 compounds: each is 2–3 L-walls arranged as a partial courtyard.
    let compounds = 2 + if rng.next() < 0.55 { 1 } else { 0 };
    for _ in 0..compounds {
        for _try in 0..36 {
            let cx = ((rng.next() as f32) * 2.0 - 1.0) * (lim - 8.0);
            let cz = ((rng.next() as f32) * 2.0 - 1.0) * (lim - 8.0);
            if near_points(
                &Aabb {
                    x0: cx - 4.0,
                    x1: cx + 4.0,
                    y0: 0.0,
                    y1: 2.0,
                    z0: cz - 4.0,
                    z1: cz + 4.0,
                },
                &anchors,
                SPAWN_CLEAR as f32,
            ) {
                continue;
            }
            let mut segs = Vec::new();
            // outer L
            segs.extend(make_l_wall(&mut rng, cx - 2.0, cz - 2.0, 1.8));
            // opposing L to form a compound mouth
            segs.extend(make_compound_l(&mut rng, cx + 1.5, cz + 1.0, 1.8));
            if group_fits(&segs, &cover) {
                cover.extend(segs);
                break;
            }
        }
    }

    // ── street-level cover (more than urban) ──────────────────────────────
    let cover_n = 14 + (rng.next() * 8.0).floor() as i32;
    let mut c_placed = 0;
    let mut c_try = 0;
    while c_placed < cover_n && c_try < 260 {
        c_try += 1;
        let kind = rng.next();
        let (w, d, h) = if kind < 0.4 {
            // low crate / barrel
            (
                0.7 + (rng.next() as f32) * 0.9,
                0.7 + (rng.next() as f32) * 0.9,
                1.0 + (rng.next() as f32) * 0.8,
            )
        } else if kind < 0.75 {
            // low wall segment
            let long = 1.6 + (rng.next() as f32) * 2.0;
            let flip = rng.next() < 0.5;
            (
                if flip { long } else { 0.4 },
                if flip { 0.4 } else { long },
                1.3 + (rng.next() as f32) * 0.6,
            )
        } else {
            // market stall footprint
            (
                1.2 + (rng.next() as f32) * 1.0,
                1.2 + (rng.next() as f32) * 1.0,
                1.5 + (rng.next() as f32) * 0.5,
            )
        };
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
            c_placed += 1;
        }
    }

    // Extra free L-walls for open-sightline cover (not full compounds).
    let extra_l = 3 + if rng.next() < 0.5 { 1 } else { 0 };
    for _ in 0..extra_l {
        for _try in 0..28 {
            let cx = ((rng.next() as f32) * 2.0 - 1.0) * (lim - 5.0);
            let cz = ((rng.next() as f32) * 2.0 - 1.0) * (lim - 5.0);
            let segs = make_l_wall(&mut rng, cx, cz, 1.6);
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
    let mut rng_pick = Mulberry32::from_seed(&format!("{seed}|desert-pick"));
    let pickups = scatter_pickups(&walls, half, &mut rng_pick, &avoid, 4);
    let accent = ACCENTS[(rng.next() * ACCENTS.len() as f64).floor() as usize];

    GameMap {
        seed: seed.to_string(),
        env: EnvKind::DesertTown,
        arena_half: half,
        walls,
        spawns,
        gates,
        billboards,
        pickups,
        accent,
    }
}

/// L-wall mirrored in orientation (opens the other way) for compound mouths.
fn make_compound_l(rng: &mut Mulberry32, cx: f32, cz: f32, h_base: f32) -> Vec<Aabb> {
    let t = 0.45;
    let len_a = snap_f32(2.5 + (rng.next() as f32) * 1.5);
    let len_b = snap_f32(2.0 + (rng.next() as f32) * 1.5);
    let h = h_base + (rng.next() as f32) * 0.4;
    let x1 = snap_f32(cx);
    let z1 = snap_f32(cz);
    vec![
        Aabb {
            x0: snap_f32(x1 - len_a),
            x1,
            y0: 0.0,
            y1: h,
            z0: snap_f32(z1 - t),
            z1,
        },
        Aabb {
            x0: snap_f32(x1 - t),
            x1,
            y0: 0.0,
            y1: h,
            z0: snap_f32(z1 - len_b),
            z1,
        },
    ]
}
