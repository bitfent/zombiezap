//! The co-op runtime map: wraps the golden-exact legacy generator and layers
//! ZombieZap's needs on top — a clustered 5-player spawn, zombie gates
//! validated for reachability, and non-colliding billboard quads. All extras
//! are deterministic from the seed and consume NO rng from the legacy stream
//! (golden compatibility stays intact by construction).

use super::grid::WalkGrid;
use super::generate_arena;
use crate::constants::MAX_PLAYERS;
use crate::types::{Aabb, EnvKind, Spawn};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gate {
    pub x: f32,
    pub z: f32,
    /// Facing into the arena (the direction spawned zombies walk).
    pub yaw: f32,
}

/// A non-colliding ad quad mounted on the perimeter, facing the arena.
/// Placement never obscures gameplay: billboards live above head height on
/// the boundary walls (spec: ads must not cover enemies or objectives).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Billboard {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Outward wall this hangs on: 0=-z wall, 1=+z, 2=-x, 3=+x.
    pub wall: u8,
    pub w: f32,
    pub h: f32,
    pub ad_slot: u8,
}

/// Everything the server and client need to run a match on a map.
#[derive(Clone, Debug)]
pub struct GameMap {
    pub seed: String,
    pub env: EnvKind,
    pub arena_half: f32,
    pub walls: Vec<Aabb>,
    pub spawns: Vec<Spawn>,
    pub gates: Vec<Gate>,
    pub billboards: Vec<Billboard>,
    /// Open ground spots inherited from the legacy generator (supply-drop /
    /// pickup locations).
    pub pickups: Vec<(f32, f32)>,
    pub accent: u32,
}

pub fn generate_map(env: EnvKind, seed: &str) -> GameMap {
    match env {
        EnvKind::Urban => urban_coop(seed),
        EnvKind::MountainTown => super::mountain::generate(seed),
        EnvKind::DesertTown => super::desert::generate(seed),
        EnvKind::SeaTown => super::sea::generate(seed),
    }
}

fn urban_coop(seed: &str) -> GameMap {
    let arena = generate_arena(seed);
    let half = super::ARENA_HALF as f32;
    let walls = arena.to_f32_walls();
    let grid = WalkGrid::rasterize(&walls, half);

    let anchor = arena.spawns[0];
    let spawns = spawn_cluster(anchor.x as f32, anchor.z as f32, anchor.yaw as f32, &grid);
    let gates = perimeter_gates(&grid, &spawns, half);
    let billboards = perimeter_billboards(seed, half);

    GameMap {
        seed: seed.to_string(),
        env: EnvKind::Urban,
        arena_half: half,
        walls,
        spawns,
        gates,
        billboards,
        pickups: arena
            .pickups
            .iter()
            .map(|p| (p.x as f32, p.z as f32))
            .collect(),
        accent: arena.accent,
    }
}

/// Five spawn points clustered around an anchor: the team starts together
/// (co-op), on walkable cells, ≥1.2 m apart. Deterministic ring scan — no rng.
pub(crate) fn spawn_cluster(ax: f32, az: f32, yaw: f32, grid: &WalkGrid) -> Vec<Spawn> {
    let mut spawns: Vec<Spawn> = vec![Spawn { x: ax, z: az, yaw }];

    // 8 compass directions without trig: unit offsets, fixed scan order
    const D: f32 = core::f32::consts::FRAC_1_SQRT_2;
    const DIRS: [(f32, f32); 8] = [
        (1.0, 0.0),
        (D, D),
        (0.0, 1.0),
        (-D, D),
        (-1.0, 0.0),
        (-D, -D),
        (0.0, -1.0),
        (D, -D),
    ];
    // ring offsets in fixed order, nearest first
    'outer: for radius in [1.5f32, 2.5, 3.5, 4.5, 6.0] {
        for &(dx, dz) in DIRS.iter() {
            if spawns.len() >= MAX_PLAYERS {
                break 'outer;
            }
            let (x, z) = (ax + dx * radius, az + dz * radius);
            if !grid.walkable_at(x, z) {
                continue;
            }
            if spawns.iter().any(|s| {
                let d2 = (s.x - x) * (s.x - x) + (s.z - z) * (s.z - z);
                d2 < 1.2 * 1.2
            }) {
                continue;
            }
            spawns.push(Spawn { x, z, yaw });
        }
    }
    // Pathological maps (shouldn't happen — spawn corners are kept clear by the
    // generator): fall back to stacking on the anchor rather than failing.
    while spawns.len() < MAX_PLAYERS {
        spawns.push(Spawn { x: ax, z: az, yaw });
    }
    spawns
}

/// Candidate gates at the middle of each perimeter wall plus the two far
/// corners; keep the ones the horde can actually walk from into the team
/// (BFS-reachable from the spawn cluster). Yaw faces the arena center.
pub(crate) fn perimeter_gates(grid: &WalkGrid, spawns: &[Spawn], half: f32) -> Vec<Gate> {
    let inset = 1.2;
    let candidates = [
        // (x, z, yaw facing center): yaw 0 faces -z, π faces +z, ±π/2 sideways
        (0.0, half - inset, 0.0),                    // south wall, walks -z
        (0.0, -half + inset, core::f32::consts::PI), // north wall, walks +z
        (half - inset, 0.0, core::f32::consts::FRAC_PI_2), // east wall, walks -x
        (-half + inset, 0.0, -core::f32::consts::FRAC_PI_2), // west wall, walks +x
        (half - inset, half - inset, core::f32::consts::FRAC_PI_4),
        (
            half - inset,
            -half + inset,
            core::f32::consts::PI - core::f32::consts::FRAC_PI_4,
        ),
    ];

    let sources: Vec<(usize, usize)> = spawns
        .iter()
        .filter_map(|s| grid.cell_of(s.x, s.z))
        .collect();
    let dist = grid.distance_field(&sources);
    let n = grid.cells_per_side;

    candidates
        .iter()
        .filter_map(|&(x, z, yaw)| {
            let (ix, iz) = grid.cell_of(x, z)?;
            // reachable = this cell (or an immediate neighbor) connects to the team
            let reachable = [
                (ix, iz),
                (ix + 1, iz),
                (ix.saturating_sub(1), iz),
                (ix, iz + 1),
                (ix, iz.saturating_sub(1)),
            ]
            .iter()
            .any(|&(cx, cz)| cx < n && cz < n && dist[cz * n + cx] != u16::MAX);
            reachable.then_some(Gate { x, z, yaw })
        })
        .collect()
}

/// Six ad slots on the perimeter walls, above head height, facing inward.
/// Deterministic jitter from a seed-derived rng that is SEPARATE from the
/// legacy stream (suffix domain-separates it).
pub(crate) fn perimeter_billboards(seed: &str, half: f32) -> Vec<Billboard> {
    let mut rng = crate::rng::Mulberry32::from_seed(&format!("{seed}|ads"));
    let mut out = Vec::with_capacity(6);
    let (w, h) = (6.0, 2.5);
    let y = 4.2; // above the tallest parapet sightline, below the wall top
    for slot in 0..6u8 {
        let wall = slot % 4;
        // jittered position along the wall, keeping clear of corners
        let along = ((rng.next() as f32) * 2.0 - 1.0) * (half - w);
        let (x, z) = match wall {
            0 => (along, -half + 0.05),
            1 => (along, half - 0.05),
            2 => (-half + 0.05, along),
            _ => (half - 0.05, along),
        };
        out.push(Billboard {
            x,
            y,
            z,
            wall,
            w,
            h,
            ad_slot: slot,
        });
    }
    out
}

/// Arena perimeter walls (four tall boundary boxes). Shared by every env.
pub(crate) fn perimeter_walls(half: f32) -> Vec<Aabb> {
    let t = super::T as f32;
    let wall_h = super::WALL_H as f32;
    let h = half;
    vec![
        Aabb {
            x0: -h - t,
            x1: h + t,
            y0: 0.0,
            y1: wall_h,
            z0: -h - t,
            z1: -h,
        },
        Aabb {
            x0: -h - t,
            x1: h + t,
            y0: 0.0,
            y1: wall_h,
            z0: h,
            z1: h + t,
        },
        Aabb {
            x0: -h - t,
            x1: -h,
            y0: 0.0,
            y1: wall_h,
            z0: -h,
            z1: h,
        },
        Aabb {
            x0: h,
            x1: h + t,
            y0: 0.0,
            y1: wall_h,
            z0: -h,
            z1: h,
        },
    ]
}

/// Axis-aligned XZ overlap test for placement.
pub(crate) fn overlaps_xz(a: &Aabb, b: &Aabb) -> bool {
    a.x0 < b.x1 && a.x1 > b.x0 && a.z0 < b.z1 && a.z1 > b.z0
}

/// Inflate an AABB in XZ by margin `m` (Y unchanged).
pub(crate) fn inflate_xz(b: &Aabb, m: f32) -> Aabb {
    Aabb {
        x0: b.x0 - m,
        x1: b.x1 + m,
        y0: b.y0,
        y1: b.y1,
        z0: b.z0 - m,
        z1: b.z1 + m,
    }
}

/// Footprint (XZ union) of a group of boxes, with synthetic Y.
pub(crate) fn footprint_of(segs: &[Aabb]) -> Aabb {
    let mut x0 = f32::INFINITY;
    let mut x1 = f32::NEG_INFINITY;
    let mut z0 = f32::INFINITY;
    let mut z1 = f32::NEG_INFINITY;
    for s in segs {
        x0 = x0.min(s.x0);
        x1 = x1.max(s.x1);
        z0 = z0.min(s.z0);
        z1 = z1.max(s.z1);
    }
    Aabb {
        x0,
        x1,
        y0: 0.0,
        y1: super::WALL_H as f32,
        z0,
        z1,
    }
}

/// Snap to half-metre grid (same spirit as urban mapgen).
#[inline]
pub(crate) fn snap_f32(v: f32) -> f32 {
    (v * 2.0).round() / 2.0
}

/// True if the box's XZ footprint is within `clear` of any spawn anchor.
pub(crate) fn near_points(b: &Aabb, points: &[(f32, f32)], clear: f32) -> bool {
    for &(px, pz) in points {
        let cx = b.x0.max(px.min(b.x1));
        let cz = b.z0.max(pz.min(b.z1));
        if (cx - px) * (cx - px) + (cz - pz) * (cz - pz) < clear * clear {
            return true;
        }
    }
    false
}

/// Place a few open pickup spots on walkable ground (deterministic).
pub(crate) fn scatter_pickups(
    walls: &[Aabb],
    half: f32,
    rng: &mut crate::rng::Mulberry32,
    avoid: &[(f32, f32)],
    count: usize,
) -> Vec<(f32, f32)> {
    let grid = WalkGrid::rasterize(walls, half);
    let lim = half - 1.5;
    let mut out = Vec::with_capacity(count);
    let mut tries = 0;
    while out.len() < count && tries < 200 {
        tries += 1;
        let x = ((rng.next() as f32) * 2.0 - 1.0) * lim;
        let z = ((rng.next() as f32) * 2.0 - 1.0) * lim;
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
    // fallbacks so callers always get `count` slots
    while out.len() < count {
        let i = out.len() as f32;
        out.push((0.0, -8.0 - i * 3.0));
    }
    out
}

/// Shared wall-run with optional door/window (used by non-urban envs).
#[derive(Clone, Copy)]
pub(crate) enum Feature {
    Solid,
    Door,
    Window,
}

pub(crate) fn wall_run(
    axis: char,
    fixed: f32,
    from: f32,
    to: f32,
    feature: Feature,
    rng: &mut crate::rng::Mulberry32,
    h: f32,
) -> Vec<Aabb> {
    let half = (super::BWALL_T as f32) / 2.0;
    let door = super::DOOR as f32;
    let sill = super::SILL_H as f32;
    let head = super::HEAD_Y as f32;
    let seg = |a: f32, b: f32, y0: f32, y1: f32| -> Aabb {
        if axis == 'x' {
            Aabb {
                x0: a,
                x1: b,
                y0,
                y1,
                z0: fixed - half,
                z1: fixed + half,
            }
        } else {
            Aabb {
                x0: fixed - half,
                x1: fixed + half,
                y0,
                y1,
                z0: a,
                z1: b,
            }
        }
    };
    if matches!(feature, Feature::Solid) || to - from < door + 1.4 {
        return vec![seg(from, to, 0.0, h)];
    }
    let span = match feature {
        Feature::Door => door,
        Feature::Window => 2.0f32.max((to - from) * 0.45).min(2.8),
        Feature::Solid => unreachable!(),
    };
    let at = snap_f32(from + span / 2.0 + 0.6 + (rng.next() as f32) * (to - from - span - 1.2));
    let a0 = at - span / 2.0;
    let a1 = at + span / 2.0;
    let mut out = Vec::new();
    if a0 - from > 0.3 {
        out.push(seg(from, a0, 0.0, h));
    }
    if to - a1 > 0.3 {
        out.push(seg(a1, to, 0.0, h));
    }
    if matches!(feature, Feature::Window) {
        out.push(seg(a0, a1, 0.0, sill));
        out.push(seg(a0, a1, head, h));
    }
    out
}

/// Small rectangular building (cabin / adobe / warehouse shell).
pub(crate) fn make_building(
    rng: &mut crate::rng::Mulberry32,
    cx: f32,
    cz: f32,
    w: f32,
    d: f32,
    h: f32,
    two_room: bool,
) -> Vec<Aabb> {
    let x0 = snap_f32(cx - w / 2.0);
    let x1 = snap_f32(cx + w / 2.0);
    let z0 = snap_f32(cz - d / 2.0);
    let z1 = snap_f32(cz + d / 2.0);
    let roof_t = super::ROOF_T as f32;
    let bwall = super::BWALL_T as f32;
    let mut sides = [
        Feature::Door,
        Feature::Window,
        if rng.next() < 0.5 {
            Feature::Door
        } else {
            Feature::Window
        },
        if rng.next() < 0.6 {
            Feature::Window
        } else {
            Feature::Solid
        },
    ];
    for i in (1..sides.len()).rev() {
        let j = (rng.next() * (i + 1) as f64).floor() as usize;
        sides.swap(i, j);
    }
    let mut out = Vec::new();
    out.extend(wall_run('x', z0, x0, x1, sides[0], rng, h));
    out.extend(wall_run('x', z1, x0, x1, sides[1], rng, h));
    out.extend(wall_run('z', x0, z0, z1, sides[2], rng, h));
    out.extend(wall_run('z', x1, z0, z1, sides[3], rng, h));
    out.push(Aabb {
        x0,
        x1,
        y0: h,
        y1: h + roof_t,
        z0,
        z1,
    });
    if two_room && w.max(d) >= 7.0 {
        if w >= d {
            let mid = snap_f32(cx + ((rng.next() as f32) - 0.5) * (w * 0.25));
            out.extend(wall_run(
                'z',
                mid,
                z0 + bwall,
                z1 - bwall,
                Feature::Door,
                rng,
                h,
            ));
        } else {
            let mid = snap_f32(cz + ((rng.next() as f32) - 0.5) * (d * 0.25));
            out.extend(wall_run(
                'x',
                mid,
                x0 + bwall,
                x1 - bwall,
                Feature::Door,
                rng,
                h,
            ));
        }
    }
    out
}

/// L-shaped low wall (street cover / compound fragment).
pub(crate) fn make_l_wall(rng: &mut crate::rng::Mulberry32, cx: f32, cz: f32, h_base: f32) -> Vec<Aabb> {
    let t = 0.45;
    let len_a = snap_f32(2.5 + (rng.next() as f32) * 1.5);
    let len_b = snap_f32(2.0 + (rng.next() as f32) * 1.5);
    let h = h_base + (rng.next() as f32) * 0.5;
    let x0 = snap_f32(cx);
    let z0 = snap_f32(cz);
    vec![
        Aabb {
            x0,
            x1: snap_f32(x0 + len_a),
            y0: 0.0,
            y1: h,
            z0,
            z1: snap_f32(z0 + t),
        },
        Aabb {
            x0,
            x1: snap_f32(x0 + t),
            y0: 0.0,
            y1: h,
            z0,
            z1: snap_f32(z0 + len_b),
        },
    ]
}

/// Wide stair run: solid steps rising along +z (walkable where step top ≤ 1 m).
/// Higher risers above 1 m block the grid like urban platforms — use openings
/// beside them for BFS paths, or keep max height ≤ 1.0 for full walkability.
pub(crate) fn make_stair_run(cx: f32, cz0: f32, width: f32, height: f32) -> Vec<Aabb> {
    let step_rise = super::STEP_RISE as f32;
    let step_run = super::STEP_RUN as f32;
    let x0 = snap_f32(cx - width / 2.0);
    let x1 = snap_f32(cx + width / 2.0);
    let n_steps = (height / step_rise).round().max(1.0) as i32;
    let mut steps = Vec::new();
    let mut z = snap_f32(cz0);
    for i in 1..=n_steps {
        let h = (i as f32) * step_rise;
        if h > height + 0.01 {
            break;
        }
        let sz0 = z;
        let sz1 = snap_f32(z + step_run);
        steps.push(Aabb {
            x0,
            x1,
            y0: 0.0,
            y1: h.min(height),
            z0: sz0,
            z1: sz1,
        });
        z = sz1;
    }
    steps
}

