//! The co-op runtime map: wraps the golden-exact legacy generator and layers
//! ZombieZap's needs on top — a clustered 5-player spawn, zombie gates
//! validated for reachability, and non-colliding billboard quads. All extras
//! are deterministic from the seed and consume NO rng from the legacy stream
//! (golden compatibility stays intact by construction).

use super::grid::WalkGrid;
use super::{Arena64, generate_arena};
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
        // M7 brings the other three; until then every env plays Urban.
        _ => urban_coop(seed),
    }
}

fn urban_coop(seed: &str) -> GameMap {
    let arena = generate_arena(seed);
    let half = super::ARENA_HALF as f32;
    let walls = arena.to_f32_walls();
    let grid = WalkGrid::rasterize(&walls, half);

    let spawns = spawn_cluster(&arena, &grid);
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

/// Five spawn points clustered around the legacy corner spawn: the team starts
/// together (co-op), on walkable cells, ≥1.2 m apart. Deterministic ring scan —
/// no rng.
fn spawn_cluster(arena: &Arena64, grid: &WalkGrid) -> Vec<Spawn> {
    let anchor = arena.spawns[0];
    let (ax, az, yaw) = (anchor.x as f32, anchor.z as f32, anchor.yaw as f32);
    let mut spawns: Vec<Spawn> = vec![Spawn { x: ax, z: az, yaw }];

    // ring offsets in fixed order, nearest first
    'outer: for radius in [1.5f32, 2.5, 3.5, 4.5, 6.0] {
        for step in 0..8 {
            if spawns.len() >= MAX_PLAYERS {
                break 'outer;
            }
            // 8 compass directions without trig: unit offsets
            const DIRS: [(f32, f32); 8] = [
                (1.0, 0.0),
                (0.7071, 0.7071),
                (0.0, 1.0),
                (-0.7071, 0.7071),
                (-1.0, 0.0),
                (-0.7071, -0.7071),
                (0.0, -1.0),
                (0.7071, -0.7071),
            ];
            let (dx, dz) = DIRS[step];
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
fn perimeter_gates(grid: &WalkGrid, spawns: &[Spawn], half: f32) -> Vec<Gate> {
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
fn perimeter_billboards(seed: &str, half: f32) -> Vec<Billboard> {
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
